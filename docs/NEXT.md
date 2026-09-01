# Where to start

**You can sign in, and this paragraph said you could not for about a week after
it stopped being true.** It is corrected here on 2026-08-26 rather than quietly
edited, because "the sentence outlived the fix" is a failure this file warns
about further down and had itself committed at the top.

Cordial signs in — the Quick Sign-in code flow and typed credentials both,
the second verified by composited screenshot on 2026-08-25 — reaches the Home
page as a signed-in user, plays, takes mouse and keyboard, opens the
in-experience web view with its JS bridge round-tripping, and puts sound out
through PipeWire or PulseAudio. Wayland is the backend; X11 is supported again
under [ADR-024](adr/ADR-024-x11-is-supported-again.md) but has no editor widget
yet, so typing there is invisible.

What is actually blocking is further down and is much narrower than "sign-in":
a startup freeze on roughly a third of launches, and the canvas going black
when a TextBox focuses inside an experience.

This file is the handover. It says what is blocking, how to work on it, and —
the part worth reading even if you are in a hurry — **what has already been
ruled out**.

## Open: the gamepad-ordinal probe ran; the pre-login shell is not a readout of `gamepadType`, 2026-08-30

`android/gamepad.rs`'s module comment proposes settling `RBX::GamepadType`'s
ordinals by announcing `CORDIAL_GAMEPAD_PROBE=1 CORDIAL_GAMEPAD_TYPE=N` and
reading the button glyphs the engine draws. That probe was run — no code
changed, since it was already wired — and it does not settle the ordinals,
because **the screens reachable pre-login draw no per-type glyphs at all.**

**What was run.** `CORDIAL_GAMEPAD=1 CORDIAL_GAMEPAD_PROBE=1
CORDIAL_GAMEPAD_TYPE=N` against the existing `target/release/cordial-run`
(commit `382fe7f`, whose `gamepad.rs`/`input.rs`/`game_activity.cpp` are
byte-identical to `origin/main` `b26bc31` — diffed before relying on it, since
no build was done for this pass under a tight memory/disk budget: `free -h`
showed 22Gi/22Gi swap in use and `/var/home` had 5.1G free). Own profile at
`XDG_DATA_HOME=~/.cache/cordial-agent-gamepad`, screenshotted with
`cordial-mcp`'s `screenshot`/`info`/`click` commands over its `devctl.sock`
directly (no MCP client was wired up for this session, so the wire protocol in
`tools/cordial-mcp.py`'s `Cordial.send` was reimplemented in nine lines rather
than routed through the MCP stdio loop).

**N swept: 0, 1, 2, 99.** All four reach `Landing` normally — same layout, same
generic rounded-rect focus outline around whichever button last had it, no
button-shape icon anywhere in the frame at 672x338 or at 1433x945. Zooming the
one region that plausibly could have hidden a glyph (the focus outline itself)
at 4x shows a plain border, nothing else. `N=99` — well outside any plausible
size for a reflected enum — reached `Landing`, presented 737 frames at a normal
60fps median and shut down cleanly on the run timer with `nothing went
unanswered`, so an out-of-range ordinal is not rejected or visibly clamped at
connect time, at least as far as this screen shows.

**This is the result the module's own "control" is for.** Four different N,
zero different glyph sets: the instrument (this screen) does not read out
`gamepadType`, which is a different finding from "the ordinals are equal" and
the module's phrasing risked being read as the latter if nobody checked.

**Why it stops at Landing, and what would go further.** `Sign In` opens a
username/password form with no controller affordance either. Reaching a screen
that plausibly does render per-type glyphs needs a signed-in profile inside a
place — Sober's own issue #584 ("Almost every single game thinks im on xbox")
and #1810 (a DualShock 4 reporting the wrong face-button layout) both describe
the *games*, not Sober's shell, getting the type wrong, which puts the glyph
readout in-experience rather than pre-login on that runtime too. `main.rs`'s
own `platform-identity.md` investigation hit the identical wall in July for an
unrelated question: "Everything above stops at the landing page. A signed-out
client never enters an experience." Getting past it needs either signed-in
credentials — which nobody running this pass has, and which AGENTS.md's
tooling policy forbids typing in on a user's behalf regardless — or the user's
own `CordialTest` profile, which is exactly the one this pass was told not to
touch (`~/.local/share/cordial/profiles/CordialTest`, its own `devctl.sock`
already on disk from a previous session).

**So the ordinal is still unknown, and the next attempt should not repeat this
one.** Either re-capture `docs/traces/` with a real pad on real Android and a
session that actually joins a place (the module's "second best"), or ask
whoever owns a signed-in test profile to run the same sweep one level deeper —
past `Sign In`, into `Home`, into a place — and screenshot there. Landing and
Login are now ruled out as the observation point; do not spend another session
re-confirming that.

## Open: "the text box isnt centred" does not reproduce anywhere this session could reach, 2026-08-30

A user reported the editor "isnt perfectly centered" when a TextBox opens.
`7ab484f`, two days old at the time of the report, changed the drawn font size
and was the obvious first suspect: a glyph scaled about the wrong origin, or a
box height still sized for the pre-fix text, would read exactly like this.

**It does not.** Measured against a running client (`--profile agent-centring`,
fresh and signed out, under sway with `tools/build-wl-holders.sh` devices held
open, `CORDIAL_DEV_CONTROL=1 CORDIAL_TRACE_TEXT=1`), across every TextBox
reachable without signing in -- the Sign In username and password fields, the
Account Recovery username/email and phone fields, and the Get One-Time Code
email field, five distinct specs across two font sizes (16 and 17) and both a
plain and a masked field:

- **Rectangle.** `cordial_textbox`'s `x y w h` matched the engine's own
  `text editor placed from engine ...` trace line exactly, to the float, on
  every one of eight samples. `x=470 y=295.00003 w=340 h=22` on the username
  field, `x=470 y=361.00003 w=304 h=22` on the password field, `x=29 y=87.5
  w=1222 h=38` and `x=29 y=151.5 w=1222 h=38` on the two Account Recovery
  boxes. `host_window.rs::set_text_overlay` rounds these straight into
  `text_layer.move_`/`set_size_request` with nothing computed in between, so
  this is what the code guarantees rather than a surprise -- but it had not
  been measured against the engine's own numbers before, and it is the first
  candidate this bug's phrasing suggests ("the box isn't where it should be").
  It is not: horizontal, vertical and size are all exact.
- **Text position within the rectangle**, measured the only way that can see
  it -- a `grim` screenshot of the composited window (never `cordial_screenshot`,
  which reads the engine's own swapchain and cannot see the GTK layer at all),
  cropped to the box and read pixel-by-pixel for ink rows and columns. On the
  username field the engine's own unfocused rendering of "testuser" centres at
  content-y=306.5 (logical box centre 306); the GTK editor's caret, focused, on
  the identical string in the identical box, centres at window-y=352.5, which
  is 306.5 plus this build's own ~46px header height to the pixel. Horizontally
  the first glyph of the engine's unfocused placeholder ("U" of
  "Username/Email/Phone") starts at the same x the GTK editor starts "testuser"
  at, again to the pixel. The masked password field's caret centred about 1.5px
  low of the box's logical centre and its caret was visibly shorter than the
  plain field's for the identical font and size -- both true, both under 2px,
  neither read as "not centred" against a screenshot.
- **`i6`/`i7`, the INFERRED xAlignment/yAlignment slots, never varied.** Every
  one of the eight samples reported `i6=0 i7=1`, the same "Left"/"Centre" pair
  `native/android_classes.cpp`'s own comment already named from the two
  Login-screen boxes. Nothing in `host_window.rs` reads either slot -- there is
  no `set_alignment`, `xalign` or `set_justify` call anywhere in the file -- so
  a box with a genuinely different alignment would draw left-aligned regardless
  of what it asked for. That is a real gap and it is exactly the shape of bug
  the report describes, but nothing reachable without joining an experience
  exercises it, so it was written down here rather than guessed at and fixed
  in the moment: `editor_font.rs`'s own note that slot 6 fits
  `Enum.Font.Legacy=0` exactly as well as `xAlignment=Left=0` meant wiring it
  up here would have been compiling in a guess, the mistake this file already
  warns against for the font slot. See the fix below for how that ambiguity
  was resolved without a live capture.

**Not reached, and why.** The most likely place for a non-default alignment is
the chat box (`/`) or a restyled TextBox inside an experience, neither of which
exists at the signed-out Landing page. Reaching either needs a signed-in
profile. `CordialTest` -- the profile AGENTS.md designates for exactly this --
was held by a live client the entire session (`pidof cordial-run` returned it
throughout, `ps` showed it launched from `cordial-shell`, i.e. a human's own
session rather than a leftover agent specimen), and no other profile on this
machine is this agent's to sign into. So the investigation stopped there:
refuted on every surface reached, unconfirmed on the one surface most likely to
show it.

**One more measured detail, worth keeping rather than losing:** the masked
password field's caret centred about 1.5px low of the box's logical centre,
and its caret was visibly shorter than the plain username field's for the
identical font, size and box height -- both true, both under 2px, and both
almost certainly GTK's own invisible-char glyph having different line metrics
in Builder Sans than a Latin letter does, rather than anything Cordial computes.
Not the reported bug, not investigated further.

## Fixed (X axis), UNVERIFIED end to end (Y axis), 2026-08-30

The slot ambiguity above is resolved. `~/Projects/mocktail`, consulted per
AGENTS.md's instruction to check it before inferring a platform contract,
implements the same `NativeTextBoxInfo.<init>` hook
(`src/jnivm/jnivm.cc:4016-4024`, Apache-2.0) and its varargs reader declares
the six int constructor arguments in order: `xAlignment, yAlignment,
textColor, font, textInputType, returnKeyType`. That is a fact about Roblox's
platform API -- the constructor's own declared parameter order -- taken and
credited rather than copied, per the line AGENTS.md draws between the idea and
the transcription; the *values* this project's own boxes hold were never
mocktail's. Applied to this struct's slots it settles all of `i6`=`xAlignment`,
`i7`=`yAlignment`, `i9`=`font` (confirming `font_slot`'s existing default
rather than merely excusing it), `i10`=`textInputType`, `i11`=`returnKeyType`.
`i6`/`i7` are renamed to `x_alignment`/`y_alignment` in `RawTextBoxInfo` and
`CordialTextBoxInfo`; `i9`/`i10`/`i11` are left numbered to keep this change to
the slots the fix needed -- see `native/android_classes.cpp`'s updated comment
for the reasoning kept in one place.

**Horizontal is a real GTK property and is applied outright.**
`gtk::Text` implements `Editable`, which has `set_alignment(xalign: f32)` --
`0.0`/`0.5`/`1.0` for Left/Center/Right, applied on every `set_text_overlay`
call alongside the family and input-purpose that already get reapplied per
box. `host_window.rs::gtk_xalign` maps the three ordinals; the default
(`Left`, the only value ever measured) is unit-tested to stay `0.0` and an
unrecognised ordinal falls back to it rather than to something arbitrary.

**Vertical has no equivalent GTK property**, because `gtk::Text` is a
single-line widget and Pango centres whatever it is given within the height it
is allocated -- which is exactly why `Center`, the only `yAlignment` this
project has ever measured, needed no code at all and still gets none:
`vertical_placement`'s `Center` arm returns `y`/`h` unchanged, so the
2026-08-30 measurement above (caret centre within 0.5px of the box's own
centre) is the regression test for that arm, not just a note. `Top`/`Bottom`
are approximated by measuring the widget's own natural line height
(`gtk::Widget::measure`, taken after the font attributes are set, not a
guessed line-height multiple of the font size) and anchoring that at the box's
own edge instead of letting the widget fill the box and self-centre.

**UNVERIFIED, and said so in the code.** No box this project has ever focused
uses anything but `yAlignment=Center`, so the `Top`/`Bottom` branches have
unit tests for their arithmetic (`host_window.rs::tests::
vertical_placement_centre_is_untouched_top_and_bottom_anchor` and
`..._clamps_a_natural_height_taller_than_the_box`) and no live measurement at
all. The next session that can reach a joined experience or a restyled TextBox
should focus one with a non-default alignment, read `xAlign`/`yAlign` off
`cordial_textbox`, and take a `grim` composited screenshot the same way this
entry did, before trusting the vertical half of this fix.

**Measured:** `cargo build --release` and `cargo test --workspace` both clean
on this change, including three new tests and the renamed fields in
`editor_font.rs`'s and `cordial-linker-sys`'s existing slot tests, which still
pass under the new names -- 156/156 in `cordial-runtime`, 332/332 in
`cordial-shell`, 0 failures workspace-wide, run twice (once at `-j4`, once at
`-j2` after a memory advisory) with the same result both times.

## Open: a camera sensitivity driven negative and persisted, 2026-08-30

Reported on Discord: *"my camera on cordial lwk just fried itself and went to a
sensetivity of like -4 or something tried restarting the game and its just
cursed"*, plus *"wasnt able to resize the window in that state"*.

**Recovery, which matters more than the diagnosis and is verified.** The value
is Roblox's own, in
`<profile>/data/files/appData/GlobalBasicSettings_13.xml`, under
`<Item class="UserGameSettings">`: `MouseSensitivity`,
`MouseSensitivityFirstPerson`, `MouseSensitivityThirdPerson` and
`GamepadCameraSensitivity`, all `1` on a healthy profile. Confirmed identical
across five profiles on this host, host and Flatpak roots alike. Close Cordial
so the lock releases, set them back to 1 or delete the file.

**Cordial does not write it.** Grepped `crates/` for `GlobalBasicSettings`,
`MouseSensitivity` and `UserGameSettings`: one hit, in `load.rs:2505-2521`,
handing the engine a directory path. The engine writes the file itself. So the
persistence in symptom 2 is fully explained by symptom 1 -- something drove
Roblox's own slider past its range and the engine saved it.

**The wheel-overshoot theory: corroborated, not measured.** `axis_to_notches`
(`wayland.rs` around 2906) divides a `wl_pointer.axis` distance by
`WHEEL_AXIS_STEP = 10.0`, and that constant's own comment is marked `INFERRED`:
"10.0 is what mutter and Weston both use ... a compositor that disagrees makes
every notch scroll by the wrong amount but still in the right direction."
`CORDIAL_WHEEL_SCALE` (`input.rs:1897`) multiplies on top.

Sober #105 corroborates the premise from outside this project: scrolling far too
fast on KDE Plasma/Wayland, with a maintainer replying that a compositor sending
overly high scroll deltas is not a Sober bug. A commenter mitigates it for menus
with `FIntScrollWheelDeltaAmount` (default 140). So one physical gesture, on a
compositor that does not use 10.0, landing on the sensitivity slider, plausibly
carries the value from 1.0 through zero into strongly negative -- matching both
the magnitude and the sign.

**That is a fit, not a measurement.** The control is still open and is cheap:
drive a known number of notches at the slider with `CORDIAL_WHEEL_SCALE=1` and
again altered, and compare how far the value moves per notch. If one notch moves
it more than one step, the bug is measured. Nobody has done it -- the run was
declined on the day for memory pressure and to avoid killing a live client.

**Ruled out:** the pointer-acceleration raw/accel split at `wayland.rs:3494`. It
affects real-time deltas only and has no path to a persisted value.

**Symptom 3, the window not resizing, is open.** One speculative candidate:
`sync_pointer_lock` (`wayland.rs:3639`) engages a Wayland pointer lock while the
engine wants first person or a camera-drag button is held, and a locked pointer
has no absolute position, which would plausibly defeat a border drag. Not
corroborated by code reading or by the Sober corpus, and not tested.

**Correcting a guess recorded earlier in this file.** The section on the freeze
notes a second reporter with *"no sensetivity option avaible"* and I suggested
here that the two might be one bug -- a value the engine cannot parse both
hiding the row and reading back as nonsense. **Sober #1399 gives a different and
more mundane cause for a missing sensitivity row:** Roblox not detecting a mouse
because it was not moved during load, fixed by moving it while loading. That
weakens the shared-cause idea without refuting it, and it should not be carried
forward as though it were a lead.

## A refused capability was invisible, and built-ins skipped consent: fixed, 2026-08-30

A user reported Discord presence not broadcasting, then found the cause
themselves: they had never granted the plugin its permissions. **The bug is not
that they had to grant them. It is that nothing anywhere said they had not.**

This is a confirmed recurrence of the class `docs/releases/v0.11.0.md` opens
with -- "FPS Flex never worked, on any machine, ever ... It never checked either
reply, so nothing said so". That release fixed FPS Flex's two specific
miscalled requests. It did not make refusals visible, so the same silence has
now swallowed a different plugin for a different reason.

Four separate things have to be wrong at once for it to be this quiet, and all
four are:

1. **The refusal goes nowhere.** `plugin_host.rs::serve()` turns a missing grant
   into `Response::Denied` and writes it back down the plugin's own pipe. No
   print, no event, no record.
2. **The plugin does check, and cannot say so.** `plugins/discord-presence/main.ts`
   inspects the reply and calls `log()` with it -- but `log()` is itself
   `call("log.write", ...)`, which needs `Capability::Log`, granted by the same
   switches the user had not touched. The report of the denial is denied, and
   the TS never inspects *that* reply. Double-silent.
3. **Even granted, the log is a bare `println!`** (`plugin_host.rs`, the
   `log.write` arm) -- invisible on a packaged or Flatpak launch with no
   terminal attached, and not written to any file.
4. **`Broker::denials()` exists to solve exactly this and is never called.**
   Grepped the whole tree: the only call site is inside `#[cfg(test)]` in
   `broker.rs` itself, in a test named
   `an_ungranted_capability_is_refused_and_recorded`. `settings.rs` contains no
   occurrence of "denied" or "denial" at all. Its own doc comment states the
   case better than this section can: *"A plugin quietly failing because it
   lacks a capability is otherwise indistinguishable from a plugin that is
   broken, and that distinction is the difference between a two-minute fix and
   an afternoon."* It was right, and it was written and then not wired up.

**Built-in plugins never see the consent prompt either.** `consent::verdict` and
`consent_body` produce the itemised sentence -- "Publish what you are playing to
Discord, where your friends can see it" -- and have exactly one production call
site, `settings.rs:1446`, in the *user-install* flow. The built-in rows around
line 2020 add themselves with `Tier::BuiltIn` and never call it. So a built-in
ships enabled, inert, with four unlabelled switches and nothing that ever told
the user they exist.

**Should the permission just ship granted?** The user thinks so. The argument
against is strong enough to record rather than settle here: `PresenceSet` is an
outbound broadcast to other people, continuously, which is not the same category
as `SHIPS_DISABLED`'s stated concern (an effect that "changes how their machine
behaves"). ADR-003's default-deny exists so nothing acts on the user's behalf
before they have read what it does. Auto-granting would be a quieter version of
what that policy was written to prevent, and would break the precedent that
every capability goes through the same itemised prompt.

The proposal that satisfies both: keep default-deny, extend the *existing*
consent flow to built-ins so enabling one asks the same itemised question, and
wire `denials()` into something a person can see. Neither needs new mechanism --
both already exist and are simply not connected.

**Fixed, 2026-08-30.** Both halves landed, default-deny untouched, and a third
instance of the same class was found and fixed alongside them.

- **Built-in consent.** A built-in's row now calls `consent::verdict` itself,
  the first time this profile's Plugins page is built and only then --
  recorded in a new `plugin-consent-seen.json` per profile
  (`consent::seen_path_in`/`has_been_asked`/`mark_asked`), because a built-in
  has no install click to hang a one-off prompt on the way a user install
  does. It reuses the itemised effects list but not `Prompt::footer`'s "It
  starts switched off" -- untrue of something that ships enabled and may
  already have been running, denied, for as long as the profile has existed
  -- so `settings.rs` gained `consent_body_for_builtin` rather than reusing
  `consent_body` verbatim. Enablement (`SHIPS_DISABLED`) is untouched: this
  prompt answers "may it do X", never "does it run at all".
- **Denials, made visible.** `Broker::denials()` is still the record, but
  nothing outside one plugin's own serving thread could ever read it -- a new
  `cordial_plugins::denials` module persists a denial to
  `plugin-denials.json` per profile the first time it happens, cleared the
  moment the matching capability is granted (`grants::set`'s callers now also
  call `denials::clear`, wired in `settings.rs`). A capability switch that has
  a live denial on record says so in its own subtitle -- "Cordial has refused
  this at least once because it was not granted" -- which is a stronger claim
  than the existing "Not allowed" line, because it means the plugin actually
  asked and was actually refused, not merely that nobody has granted it yet.
- **`log.write` survives a terminal-less launch.** One `plugin.log` per
  profile, appended to beside the existing `println!`. No rotation, no
  levels -- deliberately not a logging subsystem, only the one gap that
  mattered.
- **A third instance of the same class, found while wiring the above and not
  by design.** `start_all` read the grants file exactly once, before any
  plugin thread existed, and never again -- so a capability turned on in
  Settings had no effect on an already-running plugin until the next launch.
  Measured on a live Flatpak instance: `cordial-shell` started at 14:19:12,
  every capability `discord-presence` asked for was granted through Settings,
  and `plugin-grants.json` was written at 14:20:51 -- ninety seconds into a
  run whose broker still held the empty set it read before the file existed.
  Fixed by `plugin_host::refresh_grant`, checked by `mtime` on every request a
  plugin makes so an unchanged file costs one `stat(2)` and nothing more. It
  updates two snapshots, not one: `Broker`'s, which gates the calls a plugin
  makes, and `Listener::granted`, a second copy `flush_core_events` reads for
  *delivering* a core event, which `refresh_grant` would have left stale on
  its own -- a plugin whose `lifecycle.subscribe` started succeeding while it
  stayed permanently deaf to the `client.launch`/`client.ready` events that
  capability exists to unlock, the same bug one hop further downstream.

**Not done, and worth naming.** The Listener half of the live-reload is
exercised by review rather than by an automated test: constructing one needs
a real `Writer`, which this crate can only get from a spawned Deno process,
and adding one Deno-spawning test solely to observe a `BTreeSet` field copy
was judged the heaviest possible test for the smallest possible claim. The
`Broker` half, which shares the same `modified != last_seen` gate and the
same `grants::load` call, is tested directly.

## Open: two build-shape traps that cost a session each, 2026-08-30

**The Discord Presence plugin does not appear as a built-in under `just dev --in
toolbox`.** Reported from use, not yet diagnosed. Built-in plugins are not
embedded in the binary -- there is no `include_dir!` or `include_str!` anywhere
in `cordial-plugins` -- so they are read from a directory on disk, and a
development build that has not had `plugins/` installed beside it will list
none. Every shipping package installs them, so this is a developer-environment
trap rather than a user-facing bug, which is exactly why it will keep being
rediscovered.

**The plain host build has no web view at all, and it fails silently in the
shape this file keeps warning about.** `just client` and `just dev` on the bare
host build without `--features cordial-shell/webview,cordial-runtime/webview`,
so `dialog_in_front`, `webview_dialog_opened`/`closed` and the `sync_pointer_lock`
gate are dead code in that binary: every `openWindow` falls through to the
external browser. The client says so at startup -- `webview: built without the
'webview' feature` -- and `ldd` shows no `libwebkitgtk`. Only the `--in toolbox`
and `--in distrobox` builds, and every shipping package, link it. An agent
testing the dialog gate on a host build would be measuring nothing and would
have no way to tell.

**A third instrument that does less than its name says: `cordial_text` cannot
type punctuation.** `devctl`'s `text` verb drives `script_type`, whose
`ascii_to_evdev` (`input.rs:2512`) maps `a-z`, `1-9`, `0` and space and returns
`None` for everything else. Every other character is skipped in silence -- no
warning, no error, and `pass_key_event` is never called for it. A test that
sends `/` this way and then asserts on the suppression trace will find no trace,
conclude the guard did not fire, and pass. Found on 2026-08-30 by the agent
verifying the `/` fix, which caught it in its own harness before drawing a
conclusion from it. Use a real virtual keyboard for anything outside that table.

**And the development MCP cannot test that gate either, even on the right
build.** `cordial_click`/`cordial_move` go through `devctl.rs` to
`android::input::script_move`/`script_button`, which call `pass_mouse_move`/
`pass_mouse_button` directly. The `dialog_in_front()` check and the
`CORDIAL_TRACE_MOUSE=1` "click withheld" line live only inside the real
`wl_pointer` listener callbacks, which fire exclusively from genuine compositor
seat events. A devctl click therefore always reaches the engine whether or not a
dialog is up, and observing that demonstrates nothing about the bug. Testing it
needs a real `wl_pointer` button event -- a human, or a virtual device inside a
nested compositor per `tools/build-wl-holders.sh`. This is the same class of
error as measuring a frame rate with present counts.

## Open: the corners bleed through, and it is the opaque region again, 2026-08-28

Reported with a screenshot: at the four corners of the window, **the desktop
behind Cordial is visible** -- the actual terminal underneath, not a white or
grey fill -- against a sharp-cornered Roblox rectangle inside a rounded window.
Described first as "white stuff bleeding out, like forming a sharp corner, like
it fades in and out like roblox is getting layered as we go".

**This is the third instance of one defect, and the first two are already
fixed.** `b8ae6f7` stopped claiming the CSD drop-shadow margin was opaque --
measured under sway as `surface 1636x911` against `window 1596x871`, forty
translucent pixels per axis, all declared solid -- which presented as the window
dragging a stale halo around with it. `6e5e5a6` fixed the same mechanism for the
lowered canvas on a different compositor, where a falsely-opaque region left the
subsurface uncomposited and the window was "a flat sheet of Adwaita grey" while
the engine presented at sixty frames a second.

`refresh_opaque_region` (`crates/cordial-shell/src/host_window.rs:970-1013`) is
the one place Cordial tells the compositor which toplevel pixels are solid. In
the ordinary state it is exactly two calls: `create_rectangle` over the window's
bounds, then `subtract_rectangle` of the canvas cutout. **Nothing anywhere in
this tree computes, reads or references a corner radius** -- the only
`border-radius` in the file is 8px on `.cordial-text-fallback`, an unrelated
chrome bubble for text entry. So libadwaita clips the toplevel to a rounded
rect, those corner pixels genuinely carry alpha < 1, and Cordial declares them
opaque anyway. A compositor that trusts the hint stops repainting what is behind
them, and what stays there is whatever was on screen before Cordial mapped --
which is why it is the terminal, and why it fades as damage tracking catches up.

The rectangle is now correctly *sized* and its corners are still lying.

**The awkward part of the fix, and why it is not a one-liner.**
`gtk::cairo::Region` is `cairo_region_t`, a union of axis-aligned integer
rectangles. There is no way to subtract an arc from it, so a corner-aware
version can only approximate the curve with a staircase of small rectangles, or
inset the whole declared region by the radius and accept a slightly smaller
opaque area than is true. The second is duller and safer: an under-declared
opaque region costs a little compositing work and can never produce a stale
pixel, which is the direction this bug wants erring in.

The radius itself is the theme's, not Cordial's, so it has to be read rather
than hardcoded -- and it is zero when the window is maximised or tiled, which is
worth handling because that is how a lot of people run this.

**Not attempted here**, deliberately: the change is easy to get subtly wrong,
and the only instrument that can see it is a photograph of the window.
`cordial_screenshot` reads the engine's own swapchain and therefore cannot see
the GTK surface or anything behind the window at all -- it would show a perfect
frame while the corners were still wrong.

**Sober has nothing on this**, searched per AGENTS.md: five titles match
corner/transparency terms and the only one with matching content, #1460
"transparent/glitched window", is a broken D-Bus session under a custom window
manager. That is expected -- Sober does not put the engine in a subsurface
inside a libadwaita toplevel, so this whole class of bug is Cordial's own
(ADR-011).

**One thing ruled out on the way.** The first guess here was that
`.cordial-engine-host drawingarea { background-color: transparent; }` was making
the window see-through. It is not: the toplevel is deliberately kept opaque and
only becomes transparent under the `.cordial-canvas-below` class, while the
engine is lowered behind a dialog. The comment above that CSS says so and is
correct. Changing it would have been a fix to something that was not broken.

## Parallel APK download would be slower, measured, 2026-08-28

**Asked for, tested, and the answer is no.** The proposal was aria2 or ranged
parallel connections to speed up the 221 MiB APK fetch. Measured on this
machine over the maintainer's VPN, all within a few minutes, against the real
artefact (`2.736.1408`, 231,879,817 bytes) on APKPure's CDN:

| arm | run 1 | run 2 |
|---|---|---|
| 1 stream | 10.19 MiB/s | 10.14 MiB/s |
| 4 parallel ranged streams | 10.42 MiB/s | 10.09 MiB/s |
| 8 parallel ranged streams | 8.22 MiB/s | 8.29 MiB/s |

64 MiB per arm, interleaved `1,4,8,1,4,8` in one run so drift is spread across
the arms rather than concentrated in one. **Four streams are within noise of
one, and eight are reproducibly ~19% worse.** The link saturates on a single
connection and splitting it only adds per-connection TLS and a per-connection
302 redirect. There is nothing for aria2 to win here, and a second binary in
the dependency set to lose 19% is a bad trade.

Two facts worth keeping even though the feature is not being built. The CDN
**does** support ranges -- `206` with `content-range: bytes 0-1023/231879817` --
so this is a decision about throughput and not about capability. And it
**refuses `HEAD` with 405**, so anything that wants the length before
transferring has to use a one-byte ranged `GET`.

**Cordial's own downloader is already the fastest thing measured**: 220.1 MiB
in 13 s = **16.93 MiB/s**, and 18.00 MiB/s on a second download in the same
session, taken from its own `Progress::Fetching` stream. The whole APK arrives
in about thirteen seconds. `ureq` called directly on the same URL managed 15.16
MiB/s and `curl` 11.1--12.4 on HTTP/1.1 and 7.0--8.1 forced to HTTP/2.

**Do not read that table as "Cordial beats curl".** Only the 1/4/8 comparison
was interleaved within a single run; the client-to-client numbers were taken
minutes apart on a VPN whose throughput visibly drifted across the session, and
the ordering between them is not something these measurements establish. What
they do establish is that the existing single stream is not leaving bandwidth
on the table, which is the question that decides the feature.

**One retraction, because the mistake is the reusable part.** An earlier reading
here said Cordial's downloader managed 2.35 MiB/s against curl's 10 -- a 4x gap
that would have been the real finding. It was an artefact of the instrument.
`examples/fetch_probe.rs` fetches once per provider in its loop and then a
second time through `provider::obtain`, so the progress stream contains two
downloads; taking the slope from the first sample to the last spanned the reset
between them and divided a partial second download by the whole elapsed time.
Splitting the samples into monotonic runs gives 16.93 and 18.00. This is the
broken instrument AGENTS.md opens with, produced today, by the session that
knows the rule. Anything reading `Progress::Fetching` for a rate has to notice
`done` going backwards.

## Pointer acceleration: implemented for the unlocked cursor, 2026-08-28

**Fixed, at the end of this entry.** Everything below the fix is kept as it was
written during the investigation, because the ruled-out half of it is still
true and still worth having -- the fix that landed is the maintainer's own
diagnosis, given directly, and did not come from finishing the candidate list
below. Read the fix first if all that is wanted is what changed.

Reported by the maintainer: **"pointer acceleration doesn't apply in the Roblox
window."** Clarified by them afterwards, and this is the version to work from:

> It shouldn't ignore it, it's set on only the cursor, it should work and
> accelerate in roblox ui. It doesn't.

So the state is the **default** -- `PointerAcceleration::UnlockedCursor`, the row
Settings labels "Only the cursor" -- and the complaint is about the **cursor
moving over Roblox's own interface**, not the camera. That matters, because it
is the one case the code says cannot go wrong.

**Ruled out first, because it is what Sober's tracker points at and it does not
transfer.** Sober has this symptom reported at least five times -- #19 (the
unlocked cursor accelerating when the system has acceleration off, and
*fullscreen changing it*), #55 ("cursor sensitivity scales with window size",
answered by Sober as an artifact of enabling SDL relative mode whenever the
window was fullscreened), #2072 and #1441 (HiDPI and fractional display scale).
Every one of those is a scale factor. **Cordial applies no scale factor to
pointer coordinates anywhere.** Grepping both `crates/cordial-runtime/src` and
`crates/cordial-shell/src` for `buffer_scale`, `fractional_scale`,
`device_pixel_ratio`, `scale_factor`, `hidpi` and `dpi_scale` returns exactly one
hit, and it is the help text for `CORDIAL_DPI_SCALE` in `load.rs`. The canvas
size and the pointer coordinates delivered against it are both GTK surface-local
logical units; `wl_pointer.motion`'s `wl_fixed_t` is decoded with
`fixed_to_f32(v) = v / 256.0`, which is the fixed-point inverse and not a scale,
and `dispatch_pointer_motion` passes the pair to the engine untouched. Sober's
bugs were in machinery Cordial does not have.

**What the code does, read out of it:**

- **Cursor unlocked over the canvas.** The compositor has already run the
  position through the desktop's pointer profile before Cordial sees it, and
  `pass_mouse_move` derives its delta by subtracting the previous absolute
  position. So on this reading the desktop's acceleration reaches the engine
  here and the `PointerAcceleration` setting is never consulted --
  `pointer_acceleration()` has exactly one call site, inside
  `relative_pointer_motion`, which returns immediately unless the lock is
  active.

  **This is exactly the state the report says is broken**, so treat the
  paragraph above as what the code is arranged to do rather than as what
  happens. `shell_config.rs:300-306` goes further and says the desktop's setting
  applies to the unlocked cursor "whether Cordial likes it or not"; the report
  contradicts it, and a comment that confident about a behaviour nobody has
  measured is the shape this file exists to warn about. Nothing here has been
  observed in that state.
- **Locked.** `relative_pointer_motion` picks the unaccelerated pair unless
  `CORDIAL_POINTER_ACCEL=always`. Raw is the shipped default and is deliberate.
  Settings offers "Only the cursor" and "Cursor and camera" for it.
- **The setting only reaches a client the shell launched.** `launch.rs:397` is
  the one place `CORDIAL_POINTER_ACCEL` is set, and `just dev`, `just client`
  and a hand-run `cordial-run` set none of it. **Anybody testing this from a
  terminal is always testing the default**, whatever Settings says, and that is
  worth knowing before concluding the switch does nothing.

**The open question, half of it now measured.**
`engine_wants_pointer_lock()` polls `nativeGetMainWindowIsMouseLockedCenter`, and
`input.rs` said the direction of that call had never been confirmed: a getter
Cordial had never called, where a dead one and an idle one look identical.

Two 30-second runs on 2026-08-28 with `CORDIAL_TRACE_MOUSE=1`, on the home page,
gave the same two lines both times:

    input: nativeGetMainWindowIsMouseLockedCenter resolved
    [cordial] nativeGetMainWindowIsMouseLockedCenter() -> false

**So the getter is alive**: it resolves, it is called on every pump, it answers,
and it does not throw. The dead-symbol branch is closed and the comment in
`input.rs` has been corrected. What is still unmeasured is whether it ever
answers `true`, which needs a session in first person or shift lock and cannot
be had from the home page.

**Candidate causes, now that the report is narrowed to the unlocked cursor.**
In the order they are worth eliminating, with what separates them:

1. **The pointer is locked when it looks free.** `sync_pointer_lock` asks the
   engine every pump with no canvas gate on that half, so a `true` that arrives
   or sticks while the user is on the home page routes all motion through
   `relative_pointer_motion`, which takes the *unaccelerated* pair under this
   very setting. That would present precisely as "no acceleration in the Roblox
   UI". `CORDIAL_TRACE_MOUSE=1` prints every transition, so one traced session
   in the UI separates this from everything below -- and the two runs already
   done show `-> false` on the home page with nobody touching the mouse, which
   is suggestive and not conclusive, because neither run moved a pointer.
2. **The engine places its own cursor, and not from the absolute pair.**
   `pass_mouse_move` sends `nativePassMouseMove(x, y, dx, dy)`, and `input.rs`
   already records as its load-bearing inference that the engine acts on
   `dx`/`dy` rather than the first two. Those deltas are differences of
   already-accelerated absolutes, so acceleration should survive them -- unless
   the engine applies a gain of its own, in which case what the user is watching
   is Roblox's cursor and not the compositor's, and Cordial's coordinates are
   innocent. Distinguished by whether the compositor's own cursor is visible and
   moving normally over the same window at the same time.
3. **Stale `DisplayMetrics`, as a gain on anything the engine derives from
   screen geometry.** Measured elsewhere in this file: the engine constructs
   `DisplayMetrics` exactly once, from inside `initializeNativeCode`, and gets
   `1280x720 density=1.000 densityDpi=160` -- and `load.rs:2329` calls
   `set_display_size` only *after* `initializeNativeCode` has returned
   (load.rs:2301), so Cordial's correction can never reach that one read, at any
   resolution, windowed or fullscreen. The comment at that call site already says
   the framework layer "reported the compiled 1280x720 whatever the window was
   doing". **Whether any pointer arithmetic in the engine is derived from it is
   unestablished** -- nothing in `native/` ties mouse deltas to `DisplayMetrics`
   or to `PlatformParams.dpiScale` -- so this is a structural candidate and not a
   diagnosis.
4. **The compositor is not accelerating this window.** Cordial's own motion path
   performs no arithmetic at all, so if acceleration is genuinely absent from the
   coordinates that arrive, the cause is upstream of anything in this repository.
   Separated by comparing the cursor's speed over the Cordial window against the
   same movement over any other window in the same session.

**There is no way to observe which of these is happening**, and that is its own
defect: `pointer_acceleration()`'s answer is never printed, so neither a user nor
a developer can tell whether the setting took effect. One line under
`CORDIAL_TRACE_MOUSE=1` naming the pair in use would fix it, and this project has
lost enough time to instruments that measure nothing. Proposed, not written.

The older framing, kept because it still applies to the camera half:

- If it never answers true — still possible, since only `false` has been
  observed — **first person and shift lock never take the lock**,
  the relative path never runs there, and the setting is inert in exactly the
  two modes where camera feel matters -- while `shell_config.rs`'s comment
  claims Roblox "takes it for exactly the three camera cases".
- If it answers true when it should not, or the lock sticks, **ordinary cursor
  movement goes through the raw relative path** and acceleration is stripped
  from a cursor the user thinks is free.

**The measurement still owed.** A client run with `CORDIAL_TRACE_MOUSE=1`
entering first person and then leaving it, watching for a `-> true`. The two
runs above establish only that the getter answers at all. The control is
the same run with `CORDIAL_NO_POINTER_LOCK=1`, which deliberately still polls
and still traces so the control answers "what would it have done" rather than
only "it did nothing" -- `sync_pointer_lock` asks the engine before the gate for
exactly that reason. Everything above except the two quoted runs is source
reading and a Sober corpus search, and is labelled where it is an inference.

**One more gap, found while reading and not yet costed.** `sync_pointer_lock`
has no check of TextBox focus anywhere. If the engine's own request for a locked
pointer does not clear when a chat box takes focus, nothing in Cordial notices,
and the cursor stays captured while the user types. That is the second half of
the maintainer's sentence and it has no code behind it either way.

**Fixed, 2026-08-28, by the maintainer's own diagnosis rather than by finishing
the candidate list above:**

> just use xaccel and yaccel when not pointer locked

`relative_pointer_motion` in `wayland.rs` no longer returns immediately when
`POINTER_LOCK_ACTIVE` is false. Unlocked, it now feeds the accelerated pair
(`dx`/`dy`, the same one the locked path already knew how to read) into a new
accumulator, `input::accumulate_unlocked_delta`, instead of discarding the
event. `pass_mouse_move` -- still the only thing `dispatch_pointer_motion`
calls for `wl_pointer.motion` -- now asks `resolve_mouse_delta` to choose
between that accumulated delta and the old arithmetic difference of two
absolute positions, preferring the accumulated one when there is one.

**What stops one physical movement being counted twice**, which is the whole
of what the early-return this replaces was there to prevent: the absolute
*position* still comes from exactly one place, `wl_pointer.motion`, unchanged.
Only the *delta* is chosen, and it is chosen from exactly one source per
absolute report -- `take_pending_unlocked_delta` drains the accumulator
atomically, so a report either gets what accumulated or gets the arithmetic
fallback, never both.

**Ordering was checked rather than assumed.** The
`zwp_relative_pointer_v1.relative_motion` protocol text does not say a
`relative_motion` event for a given physical sample is written to the wire
before, after, or interleaved with the matching `wl_pointer.motion` -- there is
no framing available to settle it either, since Cordial binds `wl_pointer` at
version 1 and `wl_pointer.frame` is never sent (see `PointerListener`'s own
comment on that). So the design does not lean on an order: a relative sample
that arrives first sits in the accumulator until the absolute report drains it;
one that arrives after is not lost, it accumulates for whichever absolute
report comes next, which is a smear of at most one report and not a double
count. An absolute report with nothing accumulated at all -- a warp, a surface
enter, or a compositor with no `zwp_relative_pointer_v1` -- falls back to the
arithmetic difference, which is exactly what every unlocked report did before
this change existed, so a compositor that never sends the extension is
unaffected.

Two comments that the fix made wrong were corrected in the same change:
`relative_pointer_motion`'s early-return comment, which said acting on
unlocked motion "would double every ordinary mouse movement" -- true of the
naive version, not of the accumulate-and-drain one that replaced it -- and
`shell_config.rs`'s `PointerAcceleration` doc, which said there was "no
unaccelerated absolute to fall back to" for the unlocked cursor. There is now,
in principle; a `Never` variant is still not offered, for the reason given
where that comment now stands.

**Not fixed in the same change, because the file was out of bounds**:
`crates/cordial-shell/src/settings.rs` around line 640 gives the identical
now-superseded reasoning ("there is no unaccelerated absolute to fall back to")
in the comment above the Settings row itself, and needs the same correction
`shell_config.rs` got here. Left for whoever is editing that file next.

**Tested**: `crates/cordial-runtime/src/android/input.rs` gained
`unaccelerated_diff_is_only_a_fallback_for_a_relative_sample`, which checks
`resolve_mouse_delta`'s precedence with no global state at all -- the same
shape `vulkan.rs`'s `resolve_present_mode` test uses, for the same reason --
and `pending_unlocked_delta_sums_until_taken_and_then_is_gone`, which checks
that `accumulate_unlocked_delta` sums rather than overwrites, that taking
drains it, and that `reset_mouse_delta` discards a pending sample the way it
already discarded `MOUSE_LAST`. **This originally said the `cargo test
--release --workspace` output was "in the pull request"; there was no pull
request, this went straight to a branch, and that sent a reader looking for a
document that does not exist.** The real output is in the "Corrected on
review" entry below, pasted rather than pointed at.

**What this does not establish, and could not from this machine.** A client
run proves the code path executes and does not crash; it cannot prove the
cursor feels accelerated, because that is a human judgement about a physical
mouse, and no session here moved one. Nothing above resolves which of the four
candidate causes this file recorded earlier was the actual mechanism behind the
report -- the maintainer's fix was implemented directly rather than derived
from finishing that narrowing, so the candidate list stands as unfinished
investigation, not as an explanation for why the fix works. Whether real
compositors in practice ever deliver `relative_motion` after its matching
`wl_pointer.motion` -- as opposed to merely being permitted to by the protocol
text -- was reasoned from the specification and from the absence of
`wl_pointer.frame` at this binding's version, not observed on a running
compositor with a logging patch; if the accumulate-then-drain path turns out
never to run in practice because every compositor tested happens to send
`relative_motion` first, this is the place that inference should be checked.

**Corrected on review, 2026-08-28.** Two reviewers read the change above; seven
findings came back, six of them against source that held up and one already
answered by a comment the same change had added. What changed as a result:

- **The "smear of at most one report and not a double count" line above
  overstates it.** That is only true if a relative sample never arrives after
  its *own* physical sample's absolute report has already been drained. If it
  does -- sample A's absolute report goes out on the arithmetic fallback,
  A's own `relative_motion` turns up afterwards and accumulates, and sample
  B's absolute report then drains *A*'s leftover delta instead of computing
  its own -- A's movement is sent twice and B's is never sent at all, which is
  a double count and a drop, not a delay that leaves the total right.
  `input.rs` gained
  `a_relative_sample_delivered_after_its_own_absolute_report_corrupts_the_next_one`
  to demonstrate exactly this with no compositor involved, and
  `relative_pointer_motion`'s own comment in `wayland.rs` carries the same
  correction. **Still `INFERRED`, in both places, whether a real compositor
  ever produces this ordering for consecutive samples** -- nothing available
  here can settle that without a live compositor and a logging patch, which is
  the same gap the paragraph above already named. What is no longer claimed is
  that the design is safe against it either way.
- **A real, fixable gap, found and closed:** a `relative_motion` sample
  already sitting in `PENDING_UNLOCKED_DELTA` when a web-view dialog opens was
  never cleared. `dialog_in_front`'s early-return in `relative_pointer_motion`
  stops anything *more* accumulating once the dialog is up, but did nothing
  about a sample that arrived first -- left alone, it survived the whole
  dialog and was handed to the first real report after `webview_dialog_closed`,
  applying movement from before the dialog to a cursor position from after it.
  `webview_dialog_opened` and `webview_dialog_closed` now both forget the
  accumulator directly.
- **The `dialog_in_front` check's placement, before rather than inside the
  `POINTER_LOCK_ACTIVE` branch, was checked rather than changed.** Read on its
  own it looks like camera motion could be silently dropped while locked with
  a dialog up; tracing `WaylandWindow::pump`'s own order shows that state
  cannot occur -- `sync_pointer_lock` forces the lock off, synchronously,
  before the same `pump()` call reaches the Wayland dispatch that is the only
  way to invoke this listener, so `dialog_in_front() == true` never coincides
  with `POINTER_LOCK_ACTIVE == true` there. The comment now says why rather
  than only that.
- **Two stale comments, corrected.** `pointer_acceleration`'s own doc still
  said it was "consulted on every relative-motion event, which arrives ... while
  the pointer is locked" -- true when the unlocked case returned before
  reaching it, false now that unlocked motion reaches this function too
  (`pointer_acceleration` just is not called from that branch). And the locked
  branch's comment on `CORDIAL_POINTER_ACCEL` said the unlocked cursor "has no
  equivalent switch because it has no honest \"off\"" -- which `shell_config.rs`'s
  own `PointerAcceleration` doc already contradicted: the unaccelerated pair is
  sitting right there in the same event, and a `NeverCursor` variant would be
  "a small addition ... not a redesign". Nobody having asked for it is the
  actual reason there is no switch, not impossibility, and the comment now says
  that.
- **`forget_pending_unlocked_delta` is now called at every site that also
  calls `reset_mouse_delta`, which its own doc already claimed.** It was not:
  `window.rs`'s X11 pointer-grab release called only the latter. Harmless
  today, because only `wayland.rs`'s `relative_pointer_motion` ever writes
  `PENDING_UNLOCKED_DELTA` -- X11 has no relative-pointer source, so there was
  never anything there to forget -- but the claim was wrong regardless, and is
  fixed rather than just noted.
- **This file's own claim that `cargo test --release --workspace` output "is in
  the pull request" was wrong** -- this work went straight to a branch and no
  pull request exists. The real output is below.

```
     Running unittests src/lib.rs (target/release/deps/cordial_linker_sys-d3d7578fc0a74ce5)
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
     Running unittests src/lib.rs (target/release/deps/cordial_plugins-8efeb9a8f0fbdf7f)
test result: ok. 220 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 0.05s
     Running tests/discord_presence_plugin.rs (target/release/deps/discord_presence_plugin-d9293d643fdccd73)
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.04s
     Running tests/events_integration.rs (target/release/deps/events_integration-9bdb2ab199be9c5a)
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.03s
     Running tests/flag_inspector.rs (target/release/deps/flag_inspector-1e33d0d989b082e3)
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.03s
     Running tests/plugin_call_shapes.rs (target/release/deps/plugin_call_shapes-9467128a9b514e01)
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
     Running tests/registry_example.rs (target/release/deps/registry_example-450e7b1f6a1f7605)
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
     Running tests/roundtrip.rs (target/release/deps/roundtrip-87c0fc995a8fea80)
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.04s
     Running tests/settings_roundtrip.rs (target/release/deps/settings_roundtrip-656bccef8478b430)
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s
     Running unittests src/lib.rs (target/release/deps/cordial_runtime-c46babe52385b404)
test result: ok. 311 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.50s
     Running unittests src/bin/load.rs (target/release/deps/cordial_run-df3337dc06fabae5)
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
     Running tests/apk_read_cost.rs (target/release/deps/apk_read_cost-124096876b108917)
test result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.00s
     Running tests/gamepad_gate.rs (target/release/deps/gamepad_gate-a49174f2db62c86e)
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
     Running tests/profile_configuration.rs (target/release/deps/profile_configuration-8b46ce4abfcbd812)
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.06s
     Running unittests src/lib.rs (target/release/deps/cordial_shell-c3156df5d9254881)
test result: ok. 75 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 0.39s
     Running unittests src/main.rs (target/release/deps/cordial_shell-eff526c383cb69da)
test result: ok. 136 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.12s
     Running unittests src/lib.rs (target/release/deps/cordial_update-efc58d59d56c2db1)
test result: ok. 136 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.65s
   Doc-tests cordial_linker_sys / cordial_plugins / cordial_runtime / cordial_shell / cordial_update
   -- all "test result: ok", 0 passed or 1 ignored placeholder each, 0 failed
```

902 passed, 0 failed, 10 ignored, across every unit-test binary, integration-test binary and
doc-test crate in the workspace -- `cordial_runtime`'s 311 includes both new tests named above.
`cargo build --release` was also run on its own first and finished clean (`Finished \`release\`
profile [optimized + debuginfo] target(s)`); its only warnings were pre-existing dead-code ones in
`cordial-shell`, unrelated to this change and not touched here since that crate is out of bounds
for this pass.

**One thing observed while capturing this that is not this change's to fix.** The `cargo test`
process itself did not exit even once every crate had reported "ok" -- something under
`cordial_plugins`'s test suite starts a sandboxed `bwrap`/`deno` process (consistent with
ADR-003's plugin isolation) that inherited this shell's stdout/stderr and was still running,
holding the pipe open, well after every test result had printed. Not chased further: it is in
`cordial-plugins`, outside this pass's boundary, and the actual pass/fail result above was already
complete and readable in the file `tee` wrote before that process needed to exit.

## The plugin surface has two hosts, and the tests are on the wrong one, 2026-08-28

**Anything you verify against `crates/cordial-plugins/src/host.rs` says nothing
about what a user gets.** That file holds `Session`, `Plugin` and `Pump` -- a
complete plugin host with a bounded per-plugin queue, drop counting, core-event
publication and a shutdown flush. `Session::new` is called in exactly two
places, both of them tests in that crate: `tests/events_integration.rs:28` and
`tests/discord_presence_plugin.rs:84`. The host `cordial-run` runs is
`crates/cordial-runtime/src/plugin_host.rs`, and it is a different dispatcher.

This was found by a documentation pass that read every handler rather than
trusting the nearest comment, and it had already cost something. Two of the
three shipped plugins did nothing:

- **`fps-flex` called two methods that do not exist in that shape**:
  `settings.read` (a capability name; the method is `settings.get`) and
  `flags.set` with `{key, value}` where the handler requires `{values: {...}}`.
  Both refused, neither status checked, `CordialPresentMode` never written on
  any machine. Fixed in `45f90de`, with
  `crates/cordial-plugins/tests/plugin_call_shapes.rs` as the guard: it reads
  the shipped plugins as text and fails all four of its checks on the version
  that shipped.
- **`discord-presence` is correct and now receives three of the five core
  events**, corrected here on 2026-08-28 rather than quietly edited. This
  bullet said it "still receives nothing, because the client publishes no core
  events at all", and every sentence of that has stopped being true:
  `plugin_host::publish_core` is a second producer, reachable from the client,
  and `load.rs` publishes `client.launch`, `engine.version` and
  `client.shutdown` -- all three observed arriving at the plugin in a real
  `cordial-run`, with a control run that withheld `lifecycle.read` and received
  none of them. What survives is narrower and still worth knowing:
  `client.ready` and `window.resized` are in `core_events::ALL` and published
  by nothing, so the plugin's `READY` branch is unreachable and its presence
  reads "Starting up" for the whole session.

The general lesson is the one AGENTS.md already records about
`CORDIAL_AUDIO_HOST=oss`, which was measured by calling the selector directly
and was false in the client because three gates meant the selector never ran.
**A green test suite is evidence about whatever it called.** Before believing a
plugin-surface claim, check which host the test drove -- and prefer a client run
with the plugin enabled, which is the only instrument that answers the question
a user is asking.

What is worth doing about the structural half: either the runtime host should
use cordial-plugins' pieces rather than reimplementing them, or the pieces it
does not use should go. Two dispatchers that answer the same method names
differently will drift again, and the drift is invisible from either side.
**Half of that happened on 2026-08-28**: the core bus in the runtime host reuses
`cordial-plugins`' `Pump` rather than growing a second one, so the queue, the
drop counting and the flush are one implementation with tests on both sides of
it. `Session` and its `handle` are still the dispatcher nobody runs.

## Text entry: where the editor goes, 2026-08-25

**Solved and verified.** A focused Roblox TextBox gets an editor drawn on the
box itself, with the box's own font size and colour, and a caret in the right
place. Four boxes checked by composited screenshot on 2026-08-25: the home
search bar, the search modal it opens, and the sign-in username and password
fields. The password field masks, and nothing typed appears in the log.

Three sources of geometry, in this order, in `sync_text_overlay`:

1. **`showKeyboard`'s NativeTextBoxInfo.** Exact and free. Answers for the home
   search bar and both sign-in fields.
2. **`nativeGetTextBoxInfo`, polled at 10 Hz** while the first is unusable.
   Answers for the search modal, which is focused with `x=0 y=0 w=0 h=0`
   because the engine builds the spec before the modal has laid out, then keeps
   a correct one internally and never re-offers it. The getter returns
   `x=332 y=10 w=592 h=36` about a second later -- twice out of two probe runs
   with identical numbers, then twice more out of two full check runs.
   **A zero height is the trap:** on the pump tick right after focus it answers
   `x=596 y=10 w=42 h=0`, a sliver of the header bar caught mid-animation, with
   non-zero x, y and width to make it look like an answer.
3. **A placed bar Cordial positions itself.** Now a safety net nothing lands
   in, and untested as of this date. Kept deliberately; see the comment.

### Two claims this falsified, both of which had been written down

- *"On the sign-in page neither fires."* False since the `<init>` hook was
  corrected to the dex's fifteen-argument signature. The spec was always being
  constructed; Cordial was not capturing it, so `spec_known` was false and the
  page looked as though it volunteered nothing. The sentence outlived the fix.
- *"`nativeGetTextBoxInfo` ... returns null there, measured over three runs"*,
  read as a general claim about the call. It was measured on the sign-in page
  only, before that hook fix, and has not been re-run since -- `showKeyboard`
  now answers first there, so the getter is never reached. History, not a
  current reading.

Both are corrected in `crates/cordial-runtime/src/android/wayland.rs`.

### Building this

Needs the host toolchain, not the container:
`PATH=/home/linuxbrew/.linuxbrew/bin:$PATH cargo build --release`. `target/`'s
CMake cache is configured against that clang, and touching `native/` inside
`distrobox enter cordial` forces a reconfigure that then fails to find its
archiver. `tools/text-entry-check.sh` still launches the client inside the
container, because `cage` is only there.

### The editor is a real GTK field, as of `fd0f0c6`

`gtk::Text` -- the bare editable widget from inside a `GtkEntry` -- placed on
the focused box and owning the text. Cordial's buffer is now a mirror of it,
which is what the `CORDIAL_NO_TEXT_BUFFER` comment in `wayland.rs` has been
asking for since it was written.

Three things were reconciled to get there, and the module doc in `wayland.rs`
predicted all three:

- **Two `wl_keyboard` objects.** Cordial binds its own alongside GDK's, so both
  see every key. `dispatch_key` now returns before touching the buffer while a
  box has focus -- after `pass_key_event`, so text keys are still kept out of
  the game exactly as before.
- **Two `zwp_text_input_v3` objects.** GTK's wins: the widget holds the text,
  the caret rectangle and the surrounding context, which is all an input method
  is given. Cordial's is no longer enabled, and deliberately not destroyed, so
  whether two on one seat is tolerated stays answerable by running.
- **The input region.** The punch that lets clicks reach the engine subtracts
  the canvas, and the editor is inside the canvas, so it was subtracting the
  editor too. Its rectangle is unioned back in.

**What is measured:** real keys through a nested compositor's virtual keyboard
land in the widget and nowhere else -- four `wlrctl` calls produced
`hellohixrivals`, in order, each character once. Use `wlrctl`, not
`cordial_text`: the MCP drives Cordial's own entry points and exercises only
the path that *seeds* the widget, so it will report success whatever GTK does.

Sober, for comparison, does none of this: it has no widget toolkit but does
bind `zwp_text_input_v3` directly, so it has the IME plumbing and nothing to
paint with. That is why its equivalent bug is still open. See
[`analysis/sober-input-stack.md`](analysis/sober-input-stack.md), which also
records why a nested-compositor protocol trace must be taken under `sway`
rather than `cage`.

### The editor is now tested end to end, 2026-08-25

`tools/text-input-e2e.py`. **Thirty-one assertions, three consecutive runs, none
failed** -- typing, backspace, Home, End, insertion at the caret, shift-arrow
selection replaced by the next character, select-all overtyped, click-to-position
at both ends of the field, a clipboard round trip, Escape, refocus, and no text
key reaching the game while a box has focus. Run it with `--profile <a signed-in
one>`; it needs no human and it fails loudly rather than warning.

Three instruments in it are new, and each replaces one that could not see a
failure:

- **`sway`, not `cage`.** This is the correction to the paragraph that used to
  stand here. It said the double-insert guard was `INFERRED` because cage's
  headless seat advertises no keyboard, so Cordial never binds its own
  `wl_keyboard` and its key path never runs -- true, and the reason every
  earlier reading was taken with half the code under test switched off.
- **A virtual keyboard held open from before the client starts**
  (`tools/wl-keyboard-holder.c`, built by `tools/build-wl-holders.sh`). Under
  sway the seat then reports `"capabilities": 3` with a real
  `wlr_virtual_keyboard_v1` on it, from before `open()` until after the client
  exits. That paragraph's "the compositor then sends it no keymap and no keys"
  was cage's behaviour and is not sway's. `wlrctl` remains the wrong tool for a
  different reason: it creates its device and exits in the same breath, which
  is a race it sometimes wins.
- **The `textbox` verb on the development control socket**, and
  `cordial_textbox` in the MCP. There was no readback of typed text at all
  before: the editor's change signal prints nothing, the `text ->` trace prints
  a byte count, and `cordial_screenshot` photographs the engine's swapchain,
  which cannot see a GTK widget. It reports the editor's *actual* rectangle and
  which source placed it, not the engine's volunteered spec -- those differ, and
  the difference is the case worth testing.

**So the double-insert guard is measured, not inferred: five keys produce five
characters.** With the seat carrying a keyboard for the whole run, both Cordial's
`wl_keyboard` and GDK's see every key, and exactly one insertion happens.

**And one real defect fell out of the first full run.** Clicking the home search
bar opens a modal; the modal focuses before it has laid out, `showKeyboard`
volunteers `x=0 y=0 w=0 h=0`, and the editor was being dropped into the placed
fallback bar at the bottom of the window for the second before
`nativeGetTextBoxInfo` answers -- then jumping back up to a field ten pixels
from where it started. Two fallback placements in one focus, both visible. It is
part of what "the text field looks wrong" has been describing.
`resolve_textbox_geometry` now holds the editor at the last place the engine
vouched for, for up to 1.5s, before falling through to the bar. Measured after:
`carried` then `engine`, no `fallback`, on every run.

**Resize with the editor up is now tested, and is not broken.** This used to
say it was untested and to note that `editor_rect` is in surface coordinates
while the input region is rebuilt on stacking and placement changes only, never
on configure -- which reads as a bug waiting to happen and was reported as one
by a later audit. Measured on 2026-08-26, step 12c of `tools/text-input-e2e.py`:
with a box focused, the output really resized from 1280x800 to 1160x720, and
afterwards the box still had focus, the editor was still placed from engine
geometry rather than falling back, and typing still reached it.

The likely reason nothing breaks is that `sync_text_overlay` runs off the pump
about twenty times a second and re-reads the engine's geometry every 100ms, so
a configure heals within a tick or two without the region needing to know about
it.

**Two things that case does not establish, so do not read it as more than it
is.** The engine reported the *same* rectangle either side of the resize --
`x=332 y=10 w=564 h=36` before and after -- so this proves the editor survives a
resize and does not prove it follows a box that moves. And it says nothing about
fullscreen at exactly the output size, which is the configuration Sober #1026
fingers: only in fullscreen, only on Wayland, "stops when either is even a pixel
below that".

The case asserts the output mode actually changed before asserting anything
about the editor. The first version did not, passed, and reported a
byte-identical rectangle either side -- indistinguishable from a `swaymsg` that
silently did nothing.

## Frame pacing, measured properly, 2026-08-26

A user reported *"can't play fps games and low fps randomly"*. **On the app
shell, with input flowing, there is no such thing** -- and the way that number
was got is the point.

`tools/frame-pacing-check.py` drives the pointer for the whole measurement and
then reads the percentiles out of `frame_pacing.rs`'s ring, which holds the last
1024 inter-present intervals. Two runs, same session, same build:

    input at 59/s   p50 4.7ms   p95  15.7ms   p99   16.7ms   max   19.4ms
    idle (control)  p50 4.4ms   p95 999.9ms   p99 1001.0ms   max 1004.6ms

Nothing above 20ms in 1024 consecutive frames with input flowing. The idle run's
tail is the throttle -- 1.0 a second, exactly, which is what this file's own
warning about present counts has said since 2026-08-02 -- and note that **the
medians are identical**. An average would have shown almost no difference
between a healthy client and a client presenting one frame a second, which is
why `cordial_fps` dividing presents by wall time cannot answer this question and
the percentiles can.

**Every `p99` near 1000ms recorded during the startup-freeze surveys is that
throttle**, not a stall. Those surveys drove no input by design.

What this does not cover is the report itself, which is about being in a game.
The app shell is not a game, and reproducing "can't play fps games" means
joining one.

## Open: the editor draws one font per process, and only the slot is left, 2026-08-27

**Most of this has shipped; what is left is one capture by a person.** The
editor no longer hardcodes a single family. It reads
`assets/android/fonts/font-mappings.json` and `assets/content/fonts/*` out of
the APK the user supplied, registers every shipped face with fontconfig, and
draws each box in the family, weight and slant its font id names. What is *not*
established is which constructor slot the id is in, so that is a runtime
variable rather than a compile-time guess.

It was never cosmetic. The engine stops drawing a box's own text while it is
focused and resumes on blur, so during editing the GTK widget's glyphs *are*
the visible text.

**The engine names the font, and the mapping ships in the archive.**
`com.roblox.engine.jni.model.NativeTextBoxInfo`, the styling spec handed to
`showKeyboard`, declares an `int` field spelled exactly `font` -- read out of
`classes2.dex`, `class_data_off=0x6e33f5`, `field_idx` 4718-4732, bounded
either side by `DeviceStaticParams` and `PlatformParams` so the run is this
class alone. Fifteen fields, tally 6 I / 5 F / 4 Z, matching the constructor
descriptor `(FFFFFZIIIIIIZZZ)V`.

### Two things this section used to say that are wrong

**"A 48-entry table ... in a gapless run" was half right.** 48 entries is
right and 44-51 is gapless, but the ids are `1, 2, 6..51`: **0, 3, 4 and 5 have
no row at all.** That matters twice over. It weakens the slot-6 reading, since
an `i6` of 0 would resolve to no font file. And 3, 4 and 5 are `SourceSans`,
`SourceSansBold` and `SourceSansLight` -- the engine's own default face for an
unstyled TextLabel -- so Roblox's own Android mapping does not cover Roblox's
own default font, even though `SourceSansPro-Regular.ttf` ships in the same
archive. That is the strongest available evidence that a fallback is mandatory
rather than a nicety. Cordial does not paper over it with a hand-written row:
`3 => SourceSansPro-Regular` is knowledge about Roblox's enum rather than
something the archive says, nothing would tell us when it stopped being true,
and it is exactly the hand-maintained table this section already said must not
be written.

**"`families/BuilderSans.json` gives the family string Pango needs" is right
for that one file and wrong in general.** Pango asks fontconfig, and fontconfig
reads the font file rather than Roblox's manifest, and the two disagree.
`LegacyArimo.json` and `LegacyArial.json` both declare `"name": "Arimo
(Legacy)"` while both pointing at `Arimo-Regular.ttf`, which fontconfig reads
as family `Arimo` -- so inverting the manifests is not even a function, one
file carries three declared names. The weights disagree too:
`ComicNeue-Angular-Bold.ttf` is declared as the `Regular` face at weight 400
and fontconfig reads it as 700. `FcFreeTypeQuery` on the extracted file is
therefore what `editor_font.rs` uses, because it is by construction the answer
Pango will resolve against.

Both were checked against the user's own APK on 2026-08-27, all 48 rows, with
`fc-query` and with `FcFreeTypeQuery` through the same `dlsym` path the code
takes.

### What is shipped

`crates/cordial-runtime/src/android/editor_font.rs` builds an id-to-face table
on first use. Measured on this host, 46 distinct files:
`FcConfigAppFontAddFile` accepted **46 of 46 in 48.8 ms** and `FcFreeTypeQuery`
read family, weight and slant from all 46 in **10.7 ms** -- so the mechanism
takes many files and the whole set is cheap. It is still built lazily rather
than at launch, because it also means extracting about 7.2 MB from the archive
for a feature that only matters once a game restyles a box; `install()` still
registers one font at window creation, so a launch costs what it always did.

`TextOverlay` carries `font_family`, `font_weight` and `font_italic` per box,
with the process-wide family as the fallback. Weight is carried because family
alone is not enough: the 46 files collapse to 38 fontconfig families, and ids
46-49 are all `Builder Sans` differing only in weight, so a family-only editor
would draw Roblox's Medium, Bold and ExtraBold boxes in Regular.

An id with no row falls back and says so once, naming the number. That number
is the whole of what a bug report has to go on, and it is the only thing this
channel can ever say about a marketplace font: the class declares no `L` field,
so a custom font's family asset id -- eleven digits in the manifests
themselves -- cannot cross a Java `int` at all.

### What is left: which slot, and one capture

`CORDIAL_TEXTBOX_FONT_SLOT=<6|7|9|10|11>` selects the slot at run time and
defaults to 9; `=none` turns per-box fonts off for a control in the same
session. The default is `INFERRED` and weakly: it rests on one capture of two
Login-screen boxes in which slot 9 read 46 on both and never varied, and 46 is
the id for `BuilderSans-Regular.otf`, which the login screen visibly draws.
Consistent, but a constant is consistent with any field that happens not to
vary between two boxes on one screen. Slot 6 read 0 on both and `Enum.Font.Legacy`
is 0. Only `textColor` is genuinely pinned, by a packed ARGB value nothing else
could plausibly be.

**The experiment is one capture and it needs a person, not a change.** Focus a
TextBox in a game that restyled its font and read `cordial_textbox`, which now
reports `i6 i7 i9 i10 i11 fontSlot fontId family` in its reply; or run under
`CORDIAL_TRACE_TEXT=1`, where the `text editor placed from` line carries the
same five ints and the family beside them. Whichever int moves *and* changes
the visible glyphs is the field. The control is a second box on the same screen
at the default font, or the same box with `CORDIAL_TEXTBOX_FONT_SLOT=none`.

Take it in a game using a **marketplace** font rather than merely a restyled
one, because that settles a second question at no extra cost. If one int reads
**100** -- `Enum.Font.Unknown`, the value a GUI object's legacy `Font` takes
when its `FontFace` has no enum member -- the legacy-enum reading is confirmed
and the "custom fonts cannot cross this channel" conclusion is final. If one
reads a small number outside 1-51, a session-local table exists somewhere and
that conclusion needs revisiting.

Every capture this project holds was taken on the login screen. That is the
whole reason one observation could not separate five candidates.

`i10` is on the candidate list even though `wayland.rs` already reads it as
`textInputType` for the password mask -- that reading is itself `INFERRED` from
the same login capture, nobody has recorded observing the masking engage, and
this is the experiment that would disprove it.

`z14` was missing from the `textbox spec` trace line until 2026-08-27, so the
fifteenth slot was invisible in every capture this project holds. It is printed
now.

### Two routes that would answer it and are not taken

Reading `TextBox.FontFace` from the DataModel: in-process introspection of the
engine, ruled out permanently by ADR-001 and ADR-003.

Disassembling the constructor's `iput` order, which would give true parameter
order outright: declined on AGENTS.md's licence line. Declared shapes, call
order and descriptors are observation; the body of a method is how it
implements something. Two independent agents reached that boundary on
2026-08-27 and both stopped at it rather than crossing it.

### Not applied, and deliberately

Every row of the mapping carries a third field, `fromRbxFontRatio`, a fraction
below or equal to one that varies by font. It plainly exists to reconcile
Roblox's text sizing with Android's per font. Nothing in Cordial consumes it,
and applying it naively would shrink today's editor by 21% -- the symptom that
motivated `editor_font.rs` was shape and weight, never size, so the size the
editor draws at is a working observation and a multiplier that contradicts it
is wrong. `INFERRED`: whatever the ratio multiplies, it is not the quantity
`font_size` arrives in. Do not apply it without a measurement, and do not let
it go unnoticed either.

## Open: the canvas goes black when a TextBox focuses IN AN EXPERIENCE

**Reproduced, characterised, not fixed.** This is the top open bug and the
elimination list below is the valuable part -- do not repeat it.

Press `/` in an experience. The chat box focuses, `sync_text_overlay` lowers
the engine's subsurface so the editor can be drawn over it, and the whole
window becomes a flat sheet of `#222226` (libadwaita's dark `@window_bg_color`).
Pressing Escape raises the canvas and the game returns instantly.

What is established:

- **The engine never stops rendering.** `presents` climbed 39138 to 39321 while
  the window was blank.
- **It is state-dependent, not build-dependent.** With the same binary in the
  same session, focusing a TextBox on the sign-in page lowers the canvas and
  the page stays visible -- 498 distinct sampled colours against 1 in-game.
- **The stacking itself works.** `place_below` takes effect; that is *why* the
  game disappears.

What has been ruled out, each by measurement:

1. **GTK painting an opaque background.** Forcing the toplevel *and every
   descendant* transparent (`.cordial-canvas-below *`) changed nothing.
2. **The CSS class not being applied.** Instrumented: `engine-host=true
   canvas-below=true mapped=true surface=true`, identical on the page that
   works and the one that does not.
3. **A stale opaque region.** It was genuinely stale -- only `set_canvas_cutout`
   rewrote it, and that runs off geometry changes which do not happen when a
   box focuses. Fixed anyway (`refresh_opaque_region`), and the black remained.
4. **GTK being starved of frames by the engine.** Pumping the main context for
   200 ms before the restack changed nothing.
5. **The window's decoration shadow being declared opaque.** Real, fixed, and
   not this.

The one instrument not yet brought to bear is a Wayland protocol trace of the
in-game transition. `WAYLAND_DEBUG=1` will not survive the client long enough
to reach an experience -- four consecutive launches sat at `presents` 0 or 1,
which is the timing distortion AGENTS.md warns about. Getting that trace needs
either a way to enable the trace mid-run or a lighter filter at the source.

The suspicious correlate, unproven: the Vulkan swapchain is recreated on
entering an experience (`vkCreateSwapchainKHR ... old swapchain none` followed
by `recreated`), between the lower that works and the lower that does not.

## The one rule

**Grep the capture before disassembling anything.**

`docs/traces/waydroid-roblox-startup.log.gz` is a logcat capture of this exact
APK running on real Android. When a question comes up about what the engine
expects, that is a lookup rather than an investigation.

Over one long session, **nine consecutive conclusions drawn from reading the
stripped binary were wrong**, and every conclusion drawn from running something
held up. That record is why this rule is first.

## Read the engine's own log, always, before theorising

Roblox narrates itself to:

```
<files>/appData/logs/<version>_<timestamp>_Player_*.log
```

By default `~/.local/share/cordial/instances/default/data/files/appData/logs/`.
It names subsystems, stages, file paths and exceptions in Roblox's own words. It
is the best diagnostic in the project and it answers most questions. Two comments
in this repository once claimed FLog was unrouted; both were wrong, and nobody
had looked in `appData/`.

---

# What is blocking

## 0. SOLVED: typed text renders, in the box, 2026-08-25

**The engine sends geometry every time. Cordial could not receive it, because
the `<init>` hook was one argument short.**

`CORDIAL_JNI_TRACE=1 cargo build` prints the descriptor the engine looks up:

    Found symbol, Class=`...NativeTextBoxInfo`, StaticMethod=`<init>`,
      Signature=`(FFFFFZIIIIIIZZZ)L...NativeTextBoxInfo;`
    Call Unknown Static Function ... Method=`<init>`

Five floats, a boolean, six ints, **three** booleans. The hook had two. A
signature that never matches means `<init>` resolves to nothing, `new
NativeTextBoxInfo(...)` yields null, and the engine hands that null to
`showKeyboard` -- which is the `info=NULL` this section previously concluded
meant "the engine sends nothing". It sends everything.

With the fifteenth slot in place, clicking the home search bar:

    textbox spec from showKeyboard x=516 y=10 w=358 h=36 fontSize=20
      textColor=0xff202227

and a screenshot taken in the same second shows the caret sitting at the left
edge of that search field, in the field's own colour, with no fallback chrome.
Placement is correct when a spec arrives.

### What the platform contract actually is

The engine calls `glViewTextBoxFocused`, **stops painting the box's contents**,
and calls `showKeyboard` with a `NativeTextBoxInfo`. Android answers with a real
`EditText` styled from those numbers, and *that widget* draws the characters --
Gboard does not, no IME does. Cordial is the platform, so the editor is
Cordial's obligation rather than a workaround. Sober #99 has a maintainer
confirming the same TextBox working on the Android client and not on Sober;
Sober #987 is the same symptom, open since 2025-06-14.

### Two answers the engine gives, and only one is geometry

The home search bar reports `x=516 y=10 w=358 h=36`. The search *modal* it opens
then reports `x=0 y=0 w=0 h=0`. A zeroed spec arrives as `Some(info)`, so taking
it at face value placed a 0x0 editor -- invisible, and it defeated the fallback.
Both call sites now treat non-positive width or height as no usable geometry.

### The rest of the platform obligation

A focused box eats its own keys. `pass_key_event` was forwarding every character
to the engine's general key handler as well, so "w" in a chat box walked the
character. Measured: 12 suppressed / 0 forwarded by default, 0 / 12 with
`CORDIAL_KEYS_TO_GAME_WHILE_TYPING=1`. Escape, Tab, Enter, arrows, function keys
and modifiers still reach the game.

### How to check it without a person

`tools/text-entry-check.sh [profile] [text]`. It polls for readiness rather than
sleeping, retries the click, captures with `grim` -- `cordial_screenshot` reads
the engine's swapchain and cannot see a GTK editor at all -- and asserts each
step separately so a failure names the step. It warns rather than passes when
the geometry is zeroed, which is how the 0x0 bug was found on its first run.

## 0-prev. Why typed text is invisible: the engine sends no geometry, 2026-08-24

**Traced, not inferred.** `showKeyboard`'s fourth argument is the
`NativeTextBoxInfo` saying where the box is and how its text is drawn -- Android
places a real `EditText` from it, because the engine does not paint a focused
box's contents itself. For every box reachable from the app shell the engine
passes **null**:

    [cordial] showKeyboard: info=NULL spec_known=n/a
    [cordial] textbox spec unavailable

`nativeGetTextBoxInfo` is the same fact from the other side: exported, callable,
returns null, three runs. So there is no geometry to be had from any engine call
known here, and `android_classes.cpp`'s claim that Cordial "dropped" the spec
was wrong. Nothing is dropped; nothing arrives.

That settles the shape of the answer. **The overlay is not a workaround, it is
the platform obligation** -- and Sober's tracker says the same from outside: on
#99 a maintainer confirms the same custom TextBox *"works on the Android client
... but not on Sober"*. Same engine, same build. It works there because Android
puts a real widget up; it fails on both desktop runtimes because neither did.

Placement stays open and is bounded by the same finding: with a null spec there
is nothing to place against, so the editor goes where an on-screen keyboard's
own text would sit. **Whether an in-game `TextBox` supplies a real spec where an
app-shell React input does not is untested**, and it decides whether placement
is fixable in general or only for some boxes. That is the next thing to measure.

Two smaller things found on the way. The `<init>` hook for `NativeTextBoxInfo`
had the wrong signature -- `--dump-classes` shows libjnivm expects a
`shared_ptr<java::lang::Class>` before the five floats and ours lacked it, so it
could never have matched; corrected, and marked `INFERRED` because with a null
`info` it cannot be observed firing. And **`CORDIAL_SCRIPT` does nothing without
`CORDIAL_INSTR=1`**: four runs were read as "the click missed the button" before
that was noticed, and only a control with the change reverted -- which behaved
identically -- kept it from being blamed on the change under test.

## 0-new. A freeze specimen that arrives AFTER the home page, 2026-08-30

A user's Flatpak run, reported from Discord, froze in a shape the sections
below do not describe, and two details in it are new. No backtrace: the process
was gone by the time the log reached us, which is the one thing to fix about
how these arrive -- `cordial_backtrace` on a live frozen client settles more in
thirty seconds than a log does in a day.

It did not fail to start. It reached `HOME_PAGE_INTERACTIVE` at 11:58:37.035,
signed in, drew **185 frames**, and only then stopped:

    11:59:10.162 [android] the engine has presented nothing for 6s after 185
    frames; rect=Some((0, 0, 3440, 1440)) placed=(0, 0) setpos=1 qcommit=1.
    The pump is still running, so this is not the idle throttle.

After that, nothing but the 15-second battery poll for five solid minutes. So
the process is alive, the looper is pumping, and the engine has stopped
presenting -- which is a different animal from the sections below, where the
app shell never reaches Landing at all. Whether it is the same underlying bug
arriving later or a second one is **not established**, and merging them would
be the mistake this file exists to prevent.

The two new details, both correlations and neither a cause:

**It follows the patch downloads immediately.** The last thing the engine says
before going quiet is

    11:58:37.258 I/DataModelPatchConfigurer in-app polling: downloading patch
      _InExperiencePatch assetId=80471914653504 assetVersion=12200
    11:58:37.273 I/DataModelPatchConfigurer in-app polling: downloading patch
      _UniversalAppPatch assetId=118593852151835 assetVersion=12252

and presents stop roughly 27 seconds later. Nothing here has looked at
`DataModelPatchConfigurer` before. It is worth knowing whether a client that
never polls for patches freezes at the same rate, which is a control somebody
can actually run.

**Cordial could not name a refresh rate.** First line of the run:

    refresh: 2 outputs differ and nothing here knows which the window is on;
    not naming a current rate

Two monitors at different rates, and an ultrawide 3440x1440 surface. Every
freeze measurement recorded in this file was taken on a single-output host. The
refresh path is honest about not knowing rather than guessing, so this is not
itself a bug -- but "the engine was told nothing about the refresh rate" is an
untested variable sitting right next to a presentation stall, and it is cheap
to test by forcing a rate.

**A third detail, from a second user on 2026-08-30:** *"first i had no
sensetivity option avaible and the next second it froze"*. The mouse-sensitivity
row missing from Roblox's own settings menu, and the freeze arriving
immediately after.

That ordering is worth more than either half. Every measurement in this file
treats the freeze as presents stopping -- a rendering symptom -- but a settings
menu that fails to populate its own rows is the engine's Lua app already
failing to do work, *before* anything stops being drawn. If that holds up it
moves the search earlier, away from the swapchain and towards whatever the app
shell is doing when it stops.

Unconfirmed and second-hand, so it is written here as a report and not as an
observation. The thing to establish is whether the missing row reproduces on
its own without a freeze following, which would make it an unrelated bug, or
only ever immediately before one.

Neither of the first two is a theory yet. They are things that were true of
these runs and have never been true of a measured one.

## 0-gdb. The frozen thread is parked on an empty pipe, holds no lock, and two
## things this file says about it are wrong, 2026-09-01

Three frozen clients caught and photographed under gdb on `CordialTest`
(roughly three freezes in eleven launches). §0 below ends by asking for exactly
this -- "look for what the engine's app thread holds while it sits in
`ALooper_pollOnce`" -- and the answer is **nothing**.

**It is parked, not spinning, and that is measurable rather than inferred.**
`cordial_loopers` twice, ten seconds apart, on a frozen client:

```
engine thread  polls 7,083,545 -> 7,083,744   199 in 10s = 20/s
pump           polls       721 ->       920   199 in 10s = 20/s
process CPU    2.60s -> 2.76s                 1.6% of one core
events=9 unchanged   wakes=0   since_event 35430 -> 45430ms
```

Twenty a second is exactly `BLOCK_CEILING_MS = 50`. The seven million polls are
accumulated from startup and are not still being spent -- reading the total
without a second sample is what makes this look like a spin. **`BLOCK_CEILING_MS`
is working precisely as designed and the client is frozen anyway**, because the
thread wakes twenty times a second, finds nothing, and goes back. That is the
strongest confirmation yet of this file's own line: the work item is not late,
it is never created.

**The stack says `timeout_millis=-1`, and §0 below says it is 0.**

```
#3  epoll_wait () from /lib64/libc.so.6
#4  cordial_runtime::android::looper::looper_poll_once (timeout_millis=-1, ...)
#5  0x00007f273f30152b in ?? ()          <- libroblox, stripped
```

§0 dismisses the lost-wakeup theory on the grounds that "both threads in a
frozen client are polling with a timeout -- the pump at 50 ms, the engine's own
thread at 0". The engine's own thread is asking to block forever; the 50 ms it
actually gets is Cordial's ceiling, not the engine's request. The reason given
for ruling the theory out is therefore not a fact about this bug.

**No thread is blocked on a lock. Not one.** 73 threads in the capture, every
frame 0 in `syscall` or `__syscall_cancel_arch` -- futex waits and `epoll_wait`
-- and **zero** in `pthread_mutex_lock` or `lll_lock_wait`. So "this is a lock
and not a lost message", which §0 concludes from the `CORDIAL_STARTUP_RETRY`
call blocking, is not what a frozen client looks like. `looper.rs`'s own note
above `BLOCK_CEILING_MS` already said the same thing from an earlier capture --
"nothing else in either process was holding a lock" -- and the two documents
have been contradicting each other since. This capture agrees with `looper.rs`.

### What it is waiting on

A pipe, and both ends are inside the process:

```
fd 29 -> pipe:[92904650]   fd 30 -> pipe:[92904650]   pump's looper, ident=-2, callback
fd 31 -> pipe:[92904651]   fd 32 -> pipe:[92904651]   engine's looper, ident=1, no callback
```

The engine's app thread holds the read end of `92904651` and waits. Nothing
ever writes fd 32, and `wakes=0` says `ALooper_wake` has never been called on
that looper either -- so neither of the two ways it can return is ever taken.
**Cordial does not create these pipes**: `pipe(`/`pipe2(` appear nowhere in
`native/` or `src/android/`, so the engine made both ends and is failing to
post to its own queue.

### Where that leaves the search

The looper is exonerated. It registers what it is asked to register, blocks
when asked, wakes on the ceiling, and reports honestly; a producer inside the
engine never runs. So the cause is upstream of anything in `looper.rs`, which
is where several sessions of this file have been looking.

That agrees with the two sharpest clues already here and previously unconnected
to it: `~UgcExperienceController()` present in every healthy engine log and no
frozen one, and the second-hand report of Roblox's settings menu losing a row
immediately before a freeze. Both point at the app shell failing to do work
*before* anything stops being drawn, which is exactly what an unposted command
would look like from outside.

**The next measurement**, and it is now a narrow one: find what the engine
posts into that pipe in a healthy run and what precedes the first post. A
`strace -e write -f` filtered to the pipe's fd across a healthy and a frozen
launch would name the producer and the last thing that ran before it should
have fired.

## 0. The freeze has a reliable reproduction, 2026-08-24

**It stalls at `StartupController stage = 2`, immediately after `sync cookies
from engine`.** That much is exactly right and has now been measured at n=25.
The rest of this sentence used to read "and on a signed-in profile it does this
every time", from three consecutive launches on `CordialTest`; **it is not every
time.** On `default`, signed in, twenty-five launches with no input driven:
**nine frozen, sixteen healthy -- 36%, Wilson 95% CI 20-56%.** Three-run
agreement at a 36% base rate happens about one time in twenty, which is roughly
how often it should be believed. Use `tools/startup-freeze-survey.sh`; the
interval is wide enough that a candidate fix needs a comparable n in both arms
before it means anything.

`presents=1` is also not universal. Of the eight, six presented 1, one presented
0, and one presented **5** before stopping -- and that last one is the reason
the stall watchdog's `after N frames` is worth reading rather than skimming.

    t=2.87  [FLog::RobloxTelemetry] Lua app running status has been updated to true
    t=3.00  [FLog::NativeDM] (callback) initEngine_: ... StartupController started: stage = 2
    t=3.00  [FLog::NativeDM] (callback) initEngine_: ... sync cookies from engine.
    (nothing further, ever)

A healthy run continues from there into `CorePackages...ReactFiberCommitWork`,
registers its navigation routes, and reaches `APP_READY Landing`.

**Read `appData/logs/*_Player_*.log`. It is the engine's own narration, Cordial
writes one per run, and nothing in this repository had ever looked at it** --
two days of theories argued from Cordial's stdout while this sat on disk beside
it. Sober's equivalent carries 9722 `FLog::` lines. It is the same class of
evidence as `docs/traces/`, and unlike a trace it is per-run and already there.

### What the reproduction has already killed

Each of these looked right on three to six runs and died on the next test:

- **Present mode.** MAILBOX vs FIFO, both directions, retracted twice.
- **Focus.** A scripted loss at 3.9 s does not stop a client that is still in its
  60/s startup phase. The `visible()`-derived focus loss was a genuine bug and is
  fixed in 4f818cc, but the frozen run above shows `APP_CMD_GAINED_FOCUS` and no
  loss at all, so it is not this freeze.
- **Cookies. Dead, 2026-08-25.** This used to read: "Same profile three times:
  the run that restored **0** domains froze, both runs that restored 4 were
  healthy." Across twenty-five launches on `default`, **every single run
  restored 6 domains** -- frozen and healthy alike -- and the freeze still came
  at 32%. There is no variance left to correlate with. The original reading was
  three runs on a profile whose restore count was itself unstable, which made a
  varying number look like a cause.
- **The OTA cache.** 147 MB of `ota_rbxm_decompressed_cache` and
  `UniversalApp_cache` moved aside; froze identically on the next launch.
- **A missing Choreographer, 2026-08-25.** `Register rendering frequency during
  startup` is the first line a healthy run logs past the stall, which makes a
  frame-callback the engine never receives an obvious suspect --
  `AChoreographer_postFrameCallback` is how an Android app asks for the next
  vsync, and Cordial implements none of it. **The engine does not import it.**
  `readelf --dyn-syms` on `libroblox.so` matches `choreograph` zero times, in
  any case. `symtab.rs` lists an `AChoreographer` prefix that nothing has ever
  asked for.
- **The looper's idle backoff (`2b42e63`), 2026-08-25.** Suspected because it
  puts a 250µs sleep in a loop the engine spins on while the race is being
  decided, and because the first two arms measured 9/25 frozen with it and 3/25
  without (Fisher p=0.10, run sequentially rather than interleaved). It is
  almost certainly not the cause: gated behind `BACKOFF_AFTER_PRESENTS` -- so
  that a client which never presents 120 frames never sleeps at all -- **the
  freeze still happened on six of eleven launches.** Recorded because the
  suspicion was reasonable and the next person will have it too.
- **A lost looper wakeup -- and the experiment that replaced it with something
  better, 2026-08-25.** A healthy run starts the Lua app twice and a frozen one
  once, so `CORDIAL_STARTUP_RETRY=1` was written to ask the platform's own entry
  point, `nativeAppBridgeStartLuaAppDM`, for the second start when the engine
  stops during startup. **The call never returns.** The announcement flushes and
  no completion line ever follows it, and the pump thread joins the engine in
  being stuck, so the window stops responding at all -- strictly worse than the
  freeze. It is left in the tree, off, in the way `CORDIAL_LOOPER_BLOCK` is.
  **That failure is worth more than the fix would have been: a call that blocks
  means the wedged thread is still holding something the app bridge needs, so
  this is a lock and not a lost message.** Look for what the engine's app thread
  holds while it sits in `ALooper_pollOnce`, not for a notification nobody sent.
- **A lost looper wakeup.** The shape fits and the mechanism does not: a lost
  wake strands a thread in a *blocking* `ALooper_pollOnce`, and both threads in
  a frozen client are polling with a timeout -- the pump at 50 ms, the engine's
  own thread at 0. `BLOCK_CEILING_MS` already caps an infinite block for
  exactly this reason (`CORDIAL_LOOPER_BLOCK=1` is its control). The work item
  is not being delivered late; it is never being created.

### The marker that separates them, 2026-08-25

**`~UgcExperienceController()` is in all eight surviving healthy engine logs and
none of the six surviving frozen ones.** (The engine prunes its own log
directory, so eleven of the twenty-five were already gone when they were read;
Cordial's stdout survives for all of them.) That is the sharpest discriminator this bug has had, and
it is one line rather than a rate.

Both kinds of run log `Forcing finalize experience coordinator with state 1` and
then `finalize: Did not finalize due to state.` -- so that pair reads like the
failure and is not; it appears once in each. The difference is what happens
afterwards: on a healthy run the coordinator is destructed and the shell goes on
through `Register rendering frequency during startup`, `setStage:
(stage:InitializedLuaApp)`, `startLuaApp_` and `RenderView created[2]`. On a
frozen run the destructor never runs, `startLuaApp_` is never reached, and the
log simply stops. Frozen logs are 279-293 lines; healthy ones 607-620.

Cordial's own stdout separates them just as cleanly and is not pruned: **`[stub]
ZSTD_trace_compress_begin` appears in all sixteen healthy runs and none of the
nine frozen ones**, along with `DID_LOG_IN`, `HOME_PAGE_INTERACTIVE`,
`LUA_HOME_PAGE_LOADED` and the two `DataModelPatchConfigurer` downloads. Nothing
appears in a frozen run that does not appear in a healthy one, except Cordial's
own stall warning. **There is no failed call, no unresolved symbol and no error
anywhere in a frozen run** -- the difference is entirely what is missing.

**It is the second Lua app start that never happens, not the first.** Both kinds
of run call `nativeAppBridgeStartLuaAppDM` and reach
`initializeLuaAppWithLoggedInUser` at t=0.56-0.57s, log `Lua app running status
has been updated to true` at t=0.71-0.75, and get `TestServiceLog
[onServiceProvider]` two milliseconds later. Identical. Then at t=1.16-1.20 both
log `Forcing finalize experience coordinator with state 1` and `Did not finalize
due to state`, and there the two part: a healthy run sets the running status back
to **false**, destructs the coordinator, and calls
`initializeLuaAppWithLoggedInUser` a *second* time; a frozen run does none of it.

The same fact is visible in Cordial's own stdout, which -- unlike the engine's
log -- is never pruned: **`app ready: PlatformAccountRouter` appears twice in
all sixteen healthy runs and once in all nine frozen ones.** The shell mounts
its router once per Lua app start. That is the cheapest frozen-run detector
there is, it needs no engine log, and it is 25 for 25.

**So the engine is waiting between "stage 2" and the second `InitializedLuaApp`,
and whatever releases that destruction is the thing to find.** `load.rs`'s own
comment on the bridge order is the place to start looking: *"the engine spawns
its own 'Main' thread inside `nativeGameGlobalInit`, which independently races
through the same `StartLuaAppDM` machinery our own explicit call drives."* A
frozen client has two threads named `Main`, and the race that comment describes
is the right shape for a bug that happens on a third of launches. It is not a rendering
problem: no thread is inside a Vulkan call, and the engine's own thread sits in
`ALooper_pollOnce` at about 1% of a core, which is a wait rather than a spin.

`http://10.110.101.222:5052` -- a LAN websocket that times out after 60 s -- was
recorded here as appearing "in every `CordialTest` log and no healthy one", and
suspected for being a private address. **It appears in none of the fifty logs
from the survey above**, stdout or engine, frozen or healthy, and `default`
freezes at 36% without it. Whatever it is, the freeze does not need it.

`INFERRED`, but it probably has a name: every run above logs `[FLog::
TestServiceLog] [onServiceProvider] DebugUTPLauncherWebSocketUri='' wsOptIn=0`
at the same point. On `default` that setting is empty. A `CordialTest` whose
account or flag cache carries a non-empty `DebugUTPLauncherWebSocketUri` would
produce exactly the reported symptom -- a LAN websocket nobody configured,
attempted once, timing out. Not a mystery, and not a cause: the runs that freeze
here have it empty.

### Load is not it, and the rate drifts in bursts

Thirty launches, **strictly interleaved**, eight busy loops running through the
launch against none:

    LOAD=8   1 frozen / 15   6.7%   [1.2%, 29.8%]
    LOAD=0   5 frozen / 15  33.3%   [15.2%, 58.3%]
    Fisher two-sided p = 0.17

**Load does not raise the freeze rate**, and the point estimate runs the other
way. The theory is retired.

The more useful thing in that data is the shape rather than the arms. **All six
freezes fall in runs 6 to 18, a contiguous burst across both arms**, with runs
1-5 and 19-30 giving twenty-three consecutive healthy launches, also across both
arms (p = 0.06 for the split). The rate drifts over minutes independently of
anything being varied. That is exactly what produces "9 in 25, then 3 in 25,
then 8 consecutive healthy" with no code difference at all -- **so the earlier
arms above, including the one that made the looper backoff look guilty, are
confounded and must not be quoted as evidence about code.** Both ran one arm
after the other. Interleave.

### RETRACTED: frozen runs are not the fast ones

This section said, on the strength of thirty launches, that a run which freezes
reaches every startup milestone earlier than one that does not --
`nativeAppBridgeStartLuaAppDM` at a median 0.490s against 0.665s and so on down
the list. **It does not replicate, and it was almost certainly the burst
structure two paragraphs up wearing a disguise.**

Twenty-four more launches, eleven frozen and thirteen healthy, with the same
measurement:

    nativeAppBridgeStartLuaAppDM, engine clock   frozen 0.483   healthy 0.482
    the same event, Cordial's own clock          frozen 0.692   healthy 0.692

Identical. And the offset between the two clocks is the same in both groups
(+0.214s against +0.210s), so it is not a clock artefact either -- the two
groups simply run at the same speed.

**The cause is more specific than "drift", and worth naming exactly, because it
is a trap this comparison will set again.** The thirty launches it came from
were the `LOAD=8`-against-`LOAD=0` arms, and **`LOAD=8` slows startup by roughly
250 ms**. Five of the six frozen runs landed in the fast idle arm and fourteen
of the twenty-four healthy runs in the slow busy arm, so pooling them put the
healthy median in the empty space between two modes. Split by arm, it vanishes:

    idle arm   frozen med 0.488   healthy med 0.479
    busy arm   frozen med 0.906   healthy med 0.745

Within each arm a frozen run is, if anything, marginally *slower*. It is
Simpson's paradox, from pooling across an intervention that moves the very
quantity being compared. **A frozen-versus-healthy split is not an A/B and does
not look like it needs the interleaving warning, and it needs it more**: it has
no randomisation at all, so any variable that moves startup timing will sort
itself into the groups.

The replication above also ran an AUC test on every one of 313 stdout lines
present in both groups, and on the phase durations between them, which remove
any run-level offset. **Nothing separates them.** No line reaches |AUC-0.5| >
0.35; the best is 0.78 on the second swapchain recreate, whose ranges overlap
completely.

`CORDIAL_BRIDGE_DELAY_MS` remains, off, as the knob that would have tested the
retracted theory. It is harmless and it is the cheapest way to hold the bridge
back if a future timing idea needs testing.

### Where the two actually part company, in one clock

Cordial's stdout is timestamped by `tools/startup-freeze-survey.sh`, which puts
every line of a launch on one clock. Across twenty-four runs, **frozen and
healthy are identical to within three milliseconds** all the way through:

    0.692  Lua app DataModel started              frozen 0.692  healthy 0.692
    0.798  surface handed to the engine           frozen 0.798  healthy 0.798
    1.006  app ready: Home                        frozen 1.010  healthy 1.006
    1.051  mainWorkCallback                       frozen 1.051  healthy 1.051
    1.07   reporting surface extent to the engine frozen 1.098  healthy 1.072

and then, **in healthy runs only**:

    1.366  vkCreateSwapchainKHR (the second one)
    1.871  DID_LOG_IN
    2.895  LUA_HOME_PAGE_LOADED

A frozen run reports the new surface extent like every other run and the engine
never acts on it. Nothing Cordial does differs, at any point, by more than three
milliseconds -- so **whatever decides this is inside the engine, between the
resize being reported and the swapchain being rebuilt**, and no amount of
further staring at Cordial's own log will find it. The next instrument has to
see into that gap: the engine's own log is the only thing that does, and it
stops one line after `sync cookies from engine`.

Two corrections to how that has been put here before. Cordial's stdout is
**identical up to the divergence**, not identical outright -- a healthy run goes
on to log `DID_LOG_IN`, the home page loading, both `DataModelPatchConfigurer`
downloads and a second swapchain, and a frozen run logs the stall warning
instead. Those are consequences of the second Lua app start, not causes, but
"the logs are identical" is the wrong sentence and invites the wrong search.

### The correlation still standing

Signed-in profiles reach `Home` and freeze. Logged-out profiles reach `Landing`
and mostly do not. Every clean headless run in this repository has been logged
out. That is the axis still untested at a useful n, and testing it needs a
logged-out profile put through the same twenty-five launches -- `tools/` has the
survey harness the numbers above came from.

## 0-old. The freeze is the app shell never reaching Landing, 2026-08-24

**The best characterisation so far, and the instrument that found it is a diff
of two startup logs.** Take a frozen run and a healthy one, normalise the
numbers out of the `[roblox]`/`[android]` lines, and `comm` them. The entire
difference is two lines:

    [roblox] datamodel notification: APP_READY Landing
    [roblox] app ready: Landing

Everything else about the two startups is identical. Looking at the sequence
rather than the set makes it sharper still:

    frozen   APP_READY PlatformAccountRouter, APP_READY Startup
    healthy  APP_READY PlatformAccountRouter, APP_READY Startup,
             APP_READY PlatformAccountRouter, APP_READY Startup, APP_READY Landing

A healthy client runs the router-then-startup pair **twice** and then lands. A
frozen one runs it once and stops. Both reach `app upgrade status 0/3` and
neither goes past it. So the stall is in the engine's own app shell, after that
point, and it is a second pass that fails to happen rather than a first pass
that dies.

### What it is not, all read off a live specimen with lldb

- **Not blocked in Vulkan.** 128 threads, and the only `libvulkan_intel.so`
  frames are Mesa's own queue workers in `cnd_wait` -- idle driver threads.
  Nothing is in `vkAcquireNextImageKHR`, a fence, or a present.
- **Not executing at all.** **Zero `libroblox` frames in the whole capture.**
  Every engine thread is parked in libc. The engine is not spinning on anything,
  it is waiting.
- **Not blocked on the network.** `ss -tnp` shows two ESTAB HTTPS connections
  with `Recv-Q 0` and `Send-Q 0`, and nothing in `syn-sent`. No request is
  hanging at the socket layer, which is worth knowing because "the internet was
  slow" is the natural first guess and it does not survive.
- **Not a deadlock.** 0.007 cores. The engine's game thread sits in
  `looper_poll_once(timeout_millis=50)` with `out_events` non-null -- the
  native-activity drain-then-draw loop, polling twenty times a second and
  finding nothing to do. A client that has not been told it may draw looks
  exactly like this.

lldb unwound this one properly (109 frames past `#0`), unlike the
`__syscall_cancel_arch_end` case this file records elsewhere.

### What to do next

Find out what the second `PlatformAccountRouter` pass is waiting on. It is a
platform answer the shell needs and sometimes does not get, and Cordial is what
answers those. The `--dump-classes` surface and jnivm's unresolved-call register
are the places to look for a call that goes unanswered on a frozen run and is
answered on a healthy one -- **diff those two the same way the logs were
diffed**, because that comparison is what turned this from "presents=1" into a
named stall in four commands.

**And measure the base rate before testing any fix.** Every attempt on this bug
so far has been read off three to six runs and three of them were wrong.

## 0-prev. RETRACTED: the freeze is not the focus report, 2026-08-24

**Everything in the section below is wrong and is kept because the way it went
wrong is the point.** Two further runs, made while building the fix it
recommended, killed it:

    CORDIAL_SCRIPT=1:focus-off    focus -> false at t=3.9s,
                                  then presents/s 59.2, 60.0, 57.0, 60.0 ... to the end
    CORDIAL_SCRIPT=25:focus-off   no focus report at all -- the script never reached 25s
                                  presents/s 0.0 from t=1.0s, "presented nothing after 1 frames"

So reporting focus loss does **not** stop the engine rendering: a client told it
was unfocused at 3.9 s went straight to a full 60/s and stayed there. And a
client that froze did so from its first frame, before any focus report existed
to blame.

The six runs below split perfectly along the scripted focus states, and that
split was a coincidence -- the `focus-off` runs happened to be the ones that
froze at startup on their own. Two arms of three runs looked like a mechanism
and were sampling noise, which is the *same* error this file already records
twice: for the present mode on 2026-08-23, and for the CPU comparison on
2026-08-21. Three times now. **On this bug, six runs is not enough to separate
an arm from the base rate, and nothing under about twenty should be believed.**

### What the runs do establish

The freeze is a **startup** failure, not a stall: the client presents exactly one
frame and never gets going. A healthy run ramps to ~60/s by t=4s and then drops
to the 1.0/s idle throttle; a frozen one reads 0.0 from t=1s with a single stray
present. `after 1 frames` in the watchdog line, and `presents=1` on the live
specimen, are the same state.

And the live-specimen reading still stands, because it did not depend on the
focus theory: the engine's own game thread was **awake and polling at 20 Hz** in
`looper_poll_once(timeout_millis=50)`, at 0.01 cores, with **no thread anywhere
inside a Vulkan call**. Nothing is blocked in `vkAcquireNextImageKHR` or in the
driver. The engine is running its idle loop and never starting to draw.

The headless-versus-desktop asymmetry is also **not** established. Headless runs
froze too, once enough of them were run -- `late-1` above was headless. Earlier
clean headless runs were a small sample of an intermittent bug.

## 0-old. The freeze is the focus report, 2026-08-24 -- WRONG, see above

**An engine told it lost focus stops presenting entirely, and resumes only when
told focus came back.** Reproduced on demand, headless, with a control, twice:

    CORDIAL_SCRIPT=3:focus-off              presents/s 0.0  freeze warning   (2/2)
    CORDIAL_SCRIPT=3:focus-off,8:focus-on   presents/s 1.0  none             (2/2)
    no script                               presents/s 1.0  none

1.0/s is the documented idle throttle and is healthy; 0.0 is the freeze. So a
focus-loss that is reported and never followed by a focus-gain wedges the client
permanently, and that is exactly what a user sees: a window that is plainly
focused, with an engine that believes otherwise.

This matches the live specimen captured the same day. `presents=1`, 0.01 cores,
and -- the reading that redirected the whole investigation -- the engine's own
game thread **awake and polling at 20 Hz** in `looper_poll_once(timeout_millis=50)`,
not blocked. No thread was inside a Vulkan call at all, so nothing was stalled in
`vkAcquireNextImageKHR` or the driver. The engine was running its idle loop and
declining to draw, which is what an unfocused Android client is supposed to do.

It also explains why **every headless run in this repository has been clean while
desktop runs freeze intermittently**: a headless seat delivers no focus events, so
the report never fires. That asymmetry was visible for two days and read as luck.

`android/mod.rs` already predicted the shape of this, and its own note stands:

> **This is a diagnostic, not the fix.** If it turns out to be the cause, the
> answer is to stop reporting focus loss *while the game is still loading*
> rather than to stop reporting it at all.

What is **not** yet established is why Cordial reports the loss without the
matching gain on a real desktop. `backend_focused` returns `None` for "not
known" and leaves the last reported state alone, so a `false` published early --
before the compositor's first `wl_keyboard.enter`, or across a transition where
`wayland::focused()` goes unknown -- would never be corrected. That is the next
thing to measure, not to assume: run with `CORDIAL_INSTR=1` and read the
`focus -> ` transitions against the freeze.

Quick confirmation available to anyone holding a frozen window: alt-tab away and
back. If a real focus transition unwedges it, this is the cause.

## 0a. What 2026-08-23 settled, and what it retracted

A day with one lesson, and it is the same one as 2026-08-21: **n=1 against a
probabilistic bug is not a result, and acting on one costs more than waiting.**

### The freeze is characterised. Its cause is still open

Two clients were caught frozen at once, which is the first time there has been a
live specimen. What that settled, all read from the running processes:

    presents, read twice 8 s apart through devctl
      profile CordialTest   2198 -> 2198
      profile evr_l            1 -> 1

So it strikes after half a minute of healthy rendering and also on the very
first frame. Both processes sat at **0.01 cores** with 63 threads each and
identical thread rosters.

**It is not a deadlock.** The main thread was in `looper::pump` ->
`looper_poll_once(timeout_millis=50)` -> `epoll_wait` in both, waking twenty
times a second throughout. Nothing held a contended lock. Quoting the CPU beside
the stack is what separates that from a blocked pump, and it is why AGENTS.md
insists on it.

**It is not Wayland.** `CORDIAL_X11=1` reproduces it identically, watchdog line
and all, which exonerates the compositor and the subsurface path.

**No wake was sitting unconsumed.** Every `eventfd-count` in
`/proc/<pid>/fdinfo/*` read zero in both processes. That rules out a wake
written but not noticed; it cannot distinguish "never sent" from "drained just
before the sleep", which is what `looper::BLOCK_EXPIRED` now exists to count.

### Retracted the same day: the present mode is not the cause

One run with `CORDIAL_PRESENT_MODE=off` printed no freeze warning and one run
with the MAILBOX default froze, on both backends. **That was written up as a
lead, acted on, and was wrong.** The default was flipped to forward the engine's
own FIFO and then reverted within the hour, because the control said the
opposite:

    new default (FIFO)      froze YES, YES, no   (and one segfault)
    CORDIAL_PRESENT_MODE=mailbox   froze  no, no

MAILBOX is not implicated and `1d(ii)`'s frame-rate measurement stands
untouched. Worse, every one of those trials ran with a benchmark agent working
and other clients rendering, at load average 6 -- exactly the contamination
AGENTS.md warns makes two clients' numbers meaningless. **The user pointed out
the load before the arithmetic did.** Anyone re-opening the freeze should start
by reproducing it on a quiet machine, because it is not yet known whether
contention is a cause or a coincidence.

### Capping the infinite looper wait does not fix it

`187f7fa` bounds an infinite `ALooper_pollOnce` at 50 ms so a lost wakeup costs
one frame rather than the window. **It was verified not to fix this freeze** --
a client built with it still printed `presented nothing for 5s after 1 frames`.
It is worth having anyway and `CORDIAL_LOOPER_BLOCK=1` is the control, but do
not credit it if the freeze goes quiet for some other reason.

### A client can sit in `teardown` indefinitely

A leftover client ignored SIGTERM and stayed in `looper::teardown` ->
`cordial_game_activity_lifecycle` for twenty minutes before needing SIGKILL.
`teardown_returns_within_its_grace_period_with_no_native_handle` covers only the
case where the natives never resolve; with a real handle the loop is evidently
not bounded. Untriaged, separate from the freeze.

### mocktail does not draw text boxes. Its display code is dead

CLAUDE.md sends you to mocktail before guessing a platform contract, and that
remains right in general -- but **not for text entry**. Its
`src/runtime/roblox_text_display_state.cc` computes caret positions and password
masking into a shadow buffer that **nothing renders**: grep finds no caller
anywhere in `src/` outside the file itself, and no draw, blit, glyph or font
call inside it. The user confirmed independently that mocktail does not show
text as you type.

An agent read that file and reported that mocktail "draws the box's text and
caret itself"; that was relayed here without checking whether the code had a
caller, and it was wrong. Its `roblox_text_editor.cc` call sequence is still
readable as *design* -- `sync` then `pass_text(finished=false)` per keystroke,
`return_pressed` then `pass_text(finished=true)` at Enter -- but it is the
design of a client that demonstrably does not get text on screen. The one thing
worth keeping is the **name** of `nativePassText`'s boolean, `finished`, which
`input.rs` records as "not declared anywhere Cordial can read".

§1's conclusion is untouched, because it never depended on mocktail: the engine
does not paint a focused box's own text, established from the dex superclass
chain and confirmed by the `abc` experiment.

### Sober cannot be introspected the easy ways

For anyone planning to watch Sober rather than guess:

    /proc/<pid>/fd        DENIED   (same uid)
    /proc/<pid>/environ   DENIED
    /proc/<pid>/io        DENIED

The flatpak sandbox refuses these to the owning user, and they go through the
same `ptrace_may_access` check `PTRACE_ATTACH` uses, so **plain `gdb -p` on
Sober is expected to be refused** -- INFERRED, not yet tested, because a
successful attach stops the process and the machine's owner was playing.
`flatpak run --devel` is the documented way to get debugging permissions, and it
needs Sober restarted.

Its logs are at `~/.var/app/org.vinegarhq.Sober/data/sober/sober_logs/`, **not**
`appData/logs`, which is empty. They carry Roblox's own FLog stream and are
useful -- and they contain **zero** hits for `TextBox`, `Keyboard`,
`InputConnection`, `passText` or `syncTextbox`, so they cannot answer anything
about text entry. (A first grep of them appeared to hit, because the pattern
included `IME` and matched "runt*ime*". Beware.)

## 0b. What 2026-08-21 settled, and what it retracted

A long session with one theme: **every number trusted without a control turned
out to be wrong, and every control caught it.** Four mechanisms and two of this
project's own instruments died here. Read this before re-opening any of it.

### Cordial does not have a CPU problem. Sober has the same one

`088e69c` ("Sober and mocktail idle at 8%; Cordial burns a core") compared an
**idle** Sober against a **rendering** Cordial. In the same state there is no
difference. Per-thread from `/proc/<pid>/task/*/stat`:

    Cordial, signed out      111%   99.7% in one thread named "Main"
    Cordial, signed in       117%   99.2% in one thread named "Main"
    Sober                    120%   99.5% in one thread named "Main"
    Sober, two more samples   193%, 197%   99.5%, 99.3%, same tid throughout

Sober's total was higher in every sample. `lldb` names the thread:
`ALooper_pollOnce(timeout_millis=0, out_events/out_data non-null)` at
`looper.rs:1159`, under three unsymbolised libroblox frames and `__clone3` --
a thread **Roblox** created, not Cordial's pump, which passes a 50 ms timeout
and three nulls. That is Google's own `native-activity` idiom, where a zero
timeout while animating means drain-then-draw, and it costs nothing on a phone
because presentation paces the loop.

Dead, all measured rather than argued: the present-mode substitution
(`CORDIAL_PRESENT_MODE=fifo` gives 110.2%, with the frame rate correctly
following the output to 50 Hz -- so presentation *is* pacing the outer loop);
the display-connection registration (removing it entirely, so Cordial registers
nothing as mocktail does, gives 111.3% at 60 fps); all eight TaskScheduler flags
(108.9-112.2% against a 111.4% control); and making the syscall cheaper --
coalescing sixteen zero-timeout polls per real `epoll_wait` moved CPU not at all
and raised the poll rate fourfold, because the loop is CPU-bound and simply
spins faster. Full working in `docs/analysis/startup-and-idle-cost.md`.

**Do not copy mocktail here.** Its `ALooper_pollOnce` returns a constant and its
`ALooper_addFd` registers nothing. That is an amputation, not a fix: a looper
which answers without listening can never deliver an input event, a wake or a
surface change.

**CPU affinity is not a lever, and this is measured rather than argued.** The
idea was to keep the engine's hot thread on a P-core and push background threads
off it. The engine has already done it: every thread in the process carries an
affinity mask of `fff` -- CPUs 0-11, which on this machine is exactly the six
hyperthreaded P-cores, with the four E-cores excluded. The busiest thread sits on
cpu 7, one of the four 4900 MHz top-bin cores. **Cordial sets no affinity
anywhere** (`sched_setaffinity` appears nowhere in the tree) and an ordinary
shell on the same machine gets `ffff`, so the mask is Roblox's own choice --
which is what a client written for Android big.LITTLE would do.

There is also nothing to relieve: one hot thread against twelve logical P-cores
is not contention. Overriding the engine's own placement would be arguing with a
decision it made deliberately, on hardware where it happens to be right.

Two levers remain and both are trades rather than fixes. The focus report
(128% focused against 27.5% unfocused, at the same frame rate and the same
`ident` count) means telling the engine a focused window is unfocused. Making
`pollOnce(0)` actually wait would work where coalescing did not -- it reduces
iterations rather than their cost -- but a zero timeout is the caller saying
"do not wait", so honouring it with a sleep is the platform lying. Given Sober
ships with this, neither looks worth it.

### Flag delivery works, and there is finally a positive control

`DFIntTaskSchedulerTargetFps` caps the frame rate on the requested number:
10 -> 9.5-10.9, 15 -> 14.4-16.0, 20 -> 19.3-20.7, against an uncapped 36.5-47.2,
six runs, measured with input driven throughout. It tracks the *value*, not the
presence of an override. `FIntTaskSchedulerTargetFps` -- same name, durable
prefix -- does nothing, which shows the engine matches the whole name so a
misspelling fails detectably.

Consequences: overrides written to `CORDIAL_FLAGS` reach the engine and are acted
on; a `DFInt` override survives the settings reloader §47 describes; and every
earlier null result is now a real statement about its flag rather than an
uninterpretable one. See `flag-init.md` §49.

### Instruments retired -- check this list before trusting a number

- **`mainWorkCallback` fires exactly twice in healthy runs.** "It stopped after
  two" is not a symptom.
- **`onFlagsLoaded`'s byte count is a constant.** 1,308,253 for an `FInt` six
  characters longer, an `FString` 1,001 longer, and a document with 903 keys and
  87 KB removed.
- **Present counts without input are the idle throttle**, pinned at exactly
  1.0/s. A wedged client leaves the counter *fixed*; a throttled one ticks at
  1.0/s and wakes on input. `cordial_info` twice, a few seconds apart, is the
  test.
- **Zero CPU does not disprove a deadlock.** A futex-blocked thread burns
  nothing. Quote the process CPU beside every backtrace.
- **A screenshot that reads the recorded swapchain instead of the presented one
  returns a stale, never-drawn image** -- a uniform field, identical to six
  decimal places between runs that looked nothing alike. That was very nearly
  written up as "the engine presents blank frames". Fixed in `7d85816`; both the
  swapchain and the image index now come from `VkPresentInfoKHR`.

### The freeze was an audio deadlock. Fixed and confirmed 2026-08-22

**Everything in this subsection below was written while the freeze was
attributed to the looper, and the looper had nothing to do with it.** Kept
rather than deleted, because the observations in it are all correct and it is a
good record of how a well-instrumented investigation can point steadily at the
wrong component: every fact here about Cordial being healthy was true, and it
was true because Cordial *was* healthy.

The cause was an AB-BA deadlock between Cordial's own audio teardown and
PipeWire's thread loop, caught live with gdb on a client the user reported
frozen after leaving a game:

    Thread 53 " RBX Worker B"        Thread 16 "cordial-pipewire"
      AudioDevice::close               loop_iterate -> Impl::process
        holds lock_                      holds PipeWire's loop lock
      PlaybackStream::set_active       AudioDevice::drained
        -> pw_thread_loop_lock, waits    -> lock_, waits

Every `stream_` entry point takes the thread-loop lock, and the loop thread
calls `drained`, which takes `lock_`. So holding `lock_` across any `stream_`
call is an AB-BA against PipeWire's own loop, and it never recovers. `close()`
did exactly that. Leaving a game is what calls `close()`, which is why it fired
on game exit, and the roughly one-in-twenty rate is the race window -- the loop
thread has to be inside `drained` at that moment.

Fixed in `c7215eb` by never holding `lock_` across a call into `stream_`, and
**confirmed by reproduction**: on the user's own game exit,
`AudioDevice.close: playback stream closed.` now completes in 549 ms, where in
the frozen run that line never appeared at all. Its absence in the log, next to
a gdb stack showing a thread *inside* `AudioDevice::close`, is what proved the
diagnosis from the other side.

Three things worth carrying forward:

* **The stall detector's positive branch is no longer `INFERRED`.** It fired
  correctly on the real thing -- "the engine has presented nothing for 5s after
  5966 frames ... The pump is still running, so this is not the idle throttle"
  -- and explicitly ruled out the throttle, which is exactly what it was written
  to do.
* **Read the engine's threads before the looper's.** The pump being healthy is
  not evidence about the pump; it is evidence the fault is elsewhere. Two days
  went into the wrong half of the process because a healthy pump kept being
  re-measured.
* **lldb could not unwind this stack and gdb could.** The return address landed
  on `__syscall_cancel_arch_end+0`, a zero-size label with no unwind plan, and
  lldb stopped at frame #0. `cordial_backtrace` now detects that and retries
  with gdb (`bc04085`). lldb is not broken in general -- it out-unwinds gdb on
  an ordinary address -- but a one-frame stack is not an answer and must not be
  reported as one.

### How it was characterised while still unfixed

Cordial is healthy throughout: 74 million polls, the pump ticking at its normal
20 Hz cadence, main thread in `epoll_wait` inside `looper::pump`. The engine
stops presenting -- 42 frames in one case, 92 in another -- with **zero** Vulkan
errors, no thread blocked in the driver, and every thread idle.

It reproduced once in about twenty launches here against a machine where it
happened reliably, which is not enough to bisect with and nowhere near enough to
verify a fix against. Cold-start and warm-start rates were the same (0/6 and
0/6 with the present-mode substitution on and off).

One instance examined in detail turned out **not** to be this bug at all: it had
a 60-second connection timeout to a private address, no `AssetsManifestManager`,
no `JNINativeHelper`, and one frame drawn -- a client that never finished
starting because it could not reach the network, sitting on the one frame it
managed. Check the engine's own player log for those subsystems before calling
an instance frozen.

The pump now reports its own stall: no present for five seconds while the pump
is still running prints the geometry, the pid and the backtrace command, once,
ungated. Verified negative (an idle run produces none across 35 s); the positive
branch is `INFERRED` -- no wedge occurred while it was compiled in.

### WASD is dead after joining through the Play button, and fine via deep link

**The variable is the join path, not the focus change.** Measured 2026-08-22
with Cordial's own `devctl` socket driving `pass_key_event`, scored by RMSE
between swapchain screenshots before and after a key hold. Calibrated first: in
a healthy session a 2 s hold scores ~0.34 against an idle control of ~0.06, and
the character is visibly in a different room.

| join path | runs | W hold RMSE (idle) | movement |
|---|---|---|---|
| `--join-url roblox://experiences/start?placeId=…` | 6 | 0.353 (0.056), 0.305 (0.072), 0.337 (0.100), 0.340 (0.072) | works |
| the game tile, then **Play**, in the app shell | 4 | 0.193 (0.167), 0.047 (0.033), 0.112 (0.111) | dead |

Both places tried on both paths. The decisive pair ran back to back with a
verified single engine: deep link `0.340 / idle 0.072`, character walked into a
different corridor; Play button `0.112 / idle 0.111`, camera pixel-for-pixel
unmoved after a 4 s hold.

**No focus change is required.** Play-button runs with no alt-tab at all are
just as dead. In the dead state a right-button camera drag scores 0.361 — mouse
and camera are entirely healthy — and Space and the arrow keys are dead
alongside WASD. Delivering a large mouse drag and re-testing does not revive
movement, so "the engine re-picks a scheme from the last input type" does not
survive contact.

Everything below was closed with a control, same binary:

* **Focus reporting.** `CORDIAL_SCRIPT=12:focus-off,50:focus-on` spanning the
  whole load drives real `onWindowFocusChangedNative` both ways. Movement fine
  (0.305), presents held 60/s so the throttle was not involved either. Agrees
  with `CORDIAL_REPORT_FOCUS=0` from the opposite direction.
* **A genuine focus change**, a GTK window mapped for 35 s mid-load producing a
  real `wl_keyboard.leave`. Movement fine (0.337).
* **No mouse event during the load.** `CORDIAL_TRACE_MOUSE=1` counted exactly
  one `nativePassMouseMove` per deep-link run — the `pointer_enter` hover — and
  movement worked. The `POINTER_ON_CANVAS` hypothesis is dead.
* **Clicks during the load.** Six, spread across a deep-link join. Fine (0.178).

Two things that were believed and are not true. **The orientation lock is a
constant, not a lead**: `orientation 2 (locked: no→yes)` fires at t≈2 s in every
run including the healthy ones, and is not at spawn. **Pointer tracing already
existed** — `CORDIAL_TRACE_MOUSE=1`, `input::trace_mouse` — and was written up
here as missing.

Where to go next, in order: read `focused_textbox()` in the dead state (already
printed in the `CORDIAL_TRACE_TEXT` key line, but note `devctl key` calls
`pass_key_event` directly and bypasses `dispatch_key`, so it needs a real
keypress or the field added to that trace); instrument the divergence between
`deeplink.rs`'s `Linking.detectURL` publish and the app shell's own join; and
redo the Sober comparison **through Sober's Play button**, because the control
run against Sober was a deep-link join, which is the half that works here too.

Unexplained: the reporter experiences this as an alt-tab trigger, while the
measurements say the Play button alone is sufficient. Their "no alt-tab is fine"
observation may predate this or the failure may be intermittent.

#### Sharpened 2026-08-22, and two claims above are wrong

**A respawn fixes it, and that is the control.** Twice in one session, same
binary: six W probes over 34 s scoring 0.026–0.037 against idle 0.029–0.055,
then Escape → Respawn, then 0.263 / 0.303 / 0.242 / 0.171. Corroborated outside
RMSE entirely — the game prints a plot coordinate bottom-left, which read
`(0, 0)` through the dead window and `(0, -1)` after.

`INFERRED`: the movement controls never bind to the character the Play-button
join spawns with, and a fresh `CharacterAdded` binds them. Nothing here can see
inside the engine's Lua and ADR-001 keeps it that way.

**The dead state is much narrower than "input is dead".** In it: Escape opens
and closes the menu (0.341 / 0.335), clicks on the game's own Lua buttons work
(Shop 0.258, Podiums 0.254), the camera answers a right-drag (0.244). Only
WASD, Space and the arrow keys do nothing. So keys reach the engine, reach
CoreGui, and reach the game's own GUI.

**No text box is involved — that lead is closed.** The focus reading was added
to `pass_key_event`'s own trace (`input.rs`, and note `devctl key` bypasses
`dispatch_key`, which is why the existing line never fired for synthetic keys).
Every event in every run, dead and healthy alike, reads `focus=None gen=0`;
`showKeyboard` is never called inside an experience.

**Correction: "dead" is not always permanent.** One Play-button run came alive
on its own between 46 s and 55 s after the button — `W=0.021` then `0.151`,
then 0.07–0.21 for the next 90 s. Another was still dead past 95 s. **Any run
measured only in the first ~45 s will read as uniformly dead**, which is a trap
for anyone repeating this.

**Correction: "the camera is entirely healthy" needs a qualifier.** In Podiums
VC the camera was dead too — a 40×40 px right-drag scored 0.019 — while the
game's own GUI clicks still worked at 0.254. Probably game-specific, but the
unqualified claim above is wrong.

The engine-log divergence between the two joins, so far: `startLuaApp_` /
`continueAfterLuaAppStarted_` and `pauseLuaAppAndDestroyIfNeeded …
destroySurfaceView:true` appear on the Play path only, with
`SurfaceController[_:2]` reused rather than a second one created. Nothing is
logged at the moment a run heals itself.

Still not done: the Sober comparison **through Sober's own Play button**. Every
Sober control so far has been a deep-link join, which is the half that works in
Cordial too, so none of them has tested this.

**Practical caution for whoever measures this next.** 3440×1359 uncompressed
PNGs are 14 MB each and the scratchpad is tmpfs; a few hundred filled 6 GB of
RAM and took the shell down mid-measurement. Write captures to `~/.cache` and
delete them as they are scored. Roughly one capture in a few dozen comes back
with a truncated IDAT and must be retried, or it loses the whole curve.

### A fullscreen window over the client wedged the renderer once

Presents went 60/s to 0.0/s at t=37 s and never recovered, 80 s after focus and
visibility both returned, with `looper::pump` still spinning at 8.7 M polls/s
and 92–133% CPU. Occluding *after* spawn did not do it. **One occurrence, and
that run had a second engine alive on the same profile**, so it is a lead and
not a result. Distinct from the audio deadlock above: the pump is spinning here,
not blocked.

### Roblox exposes no accessibility tree

Settled four ways, see §4 and `native/accessibility.cpp`. Any development or
automation surface has to work in coordinates and pixels.

## 1. Text entry — the last step before you can sign in

**The login form works.** Clicking Sign In on the landing page opens Roblox's
Lua-rendered login form — username, password with a reveal toggle, Quick Sign-in,
Forgot Password. Clicking a field focuses it and shows a caret. All of that is
verified by screenshot.

**What does not work is typing into it.** Characters do not appear. Everything
else about sign-in is now reachable, so this is the single remaining step.

The cause is almost certainly the same shape as the input bug it came out of.
Roblox reads text through its own on-screen-keyboard contract, and Cordial
implements none of the reverse half:

```text
nativeGetTextBoxInfo()                     -> NativeTextBoxInfo
syncTextboxTextAndCursorPosition2(String, I)
updateKeyboardSize(...)
nativeReturnPressedFromOnScreenKeyboard()
GameActivity.setSoftKeyboardActive(Z, I)   <- engine calls INTO Java
```

`nativePassText` and `nativePassKeyEvent` are already wired and are not enough on
their own. The engine calls *out* to Java to raise a soft keyboard and attach an
input connection, and nothing answers — so the field is focused with no text
source attached. Implementing that reverse contract is the job.

**Already ruled out, do not redo:** delivering keys through AGDK's
`onKeyDownNative` (accepted, ignored), delivering editing state through AGDK's
`onTextInputEventNative` with a populated `gametextinput/State` (accepted,
ignored), and re-sending window focus after the Lua app is up (no effect).

### What has since been established

Most of the reverse contract above is now wired, and the picture is much
narrower than "nothing answers".

**`showKeyboard`'s first argument is the handle of the box being edited.** It was
being discarded and text was then sent with handle 0, which the engine drops in
silence. Captured now, along with the box's current contents. `nativePassText`,
`syncTextboxTextAndCursorPosition2` and `updateKeyboardSize` are all driven and
all return without error, and `CORDIAL_TRACE_TEXT=1` shows focus detected and
text accumulating correctly. **It still does not appear in the field.**

**`updateKeyboardSize(visible=true)` destroys focus. This is the important one.**
The trace order is not ambiguous:

```text
textbox focused handle=139759059370112
updateKeyboardSize(visible=true)
textbox blurred
updateKeyboardSize(visible=false)
textbox focused handle=139759059370112
```

Focus bounces continuously while that call is driven. With
`CORDIAL_NO_KEYBOARD_REPORT=1` it is stable — one `focused`, no blur, confirmed
by control in the same session.

That also explains the field appearing to clear on every keystroke:
`edit_text_buffer` reseeds from the engine whenever the focus generation changes,
so a bouncing focus resets the buffer to empty between characters. The clearing
was self-inflicted, not the engine rejecting anything.

**Do not conclude `updateKeyboardSize` is useless and delete it.** The engine
asks for a keyboard, so something is expected to acknowledge one. What is wrong
is a bare `visible=true` with a zero-height rectangle at the window's bottom edge
— plausibly the engine re-lays-out around the reported keyboard and drops the
capture in the process. The call needs different arguments, or a different
moment, not removal.

**Ruled out as the cause of the bounce:** duplicate pointer delivery. Both AGDK's
`onTouchEventNative` and `NativeInputInterface` receive every click, so one press
does arrive twice — but disabling AGDK's copy (`CORDIAL_NO_AGDK_TOUCH=1`) leaves
the bounce exactly as it was.

### Where this is going

Synthesising an input method by hand is the wrong shape and is being abandoned.
Cordial becomes a **bridge**: the platform's own input method on one side,
Android's contract on the other. On Wayland that is `zwp_text_input_v3`, which
the compositor routes to whatever the user actually runs — ibus, fcitx,
squeekboard on a phone — so composition, dead keys and CJK candidate windows stop
being Cordial's to reimplement badly.

The Android half does not go away: the engine only speaks `showKeyboard` and
friends, because it is the Android build. What goes away is Cordial inventing the
editing state in the middle. See
[ADR-011](adr/ADR-011-wayland-and-libadwaita.md).

### The Android half is bigger than `showKeyboard` — AGDK's `InputConnection`

**Correction to "the engine only speaks `showKeyboard` and friends."** A live
run's jnivm log shows the engine also reaching for
`InputConnection.setState`/`setSoftKeyboardActive`/`restartInput` and getting
`Constructed Unresolved symbol` every time — the *outbound* half of AGDK's own
`GameTextInput` contract, engine calling out to report its own idea of the
editing state, as distinct from the *inbound* half
(`onTextInputEventNative`) already ruled out above. Nothing had ever
constructed an `InputConnection` object for the engine to call these on, so
every one of these calls landed on a receiver that did not exist.

Implemented in `native/game_activity.cpp`:

- `InputConnection` (`com/google/androidgamesdk/gametextinput/InputConnection`),
  constructed once and handed to the engine via
  `GameActivity.setInputConnectionNative` — driven directly from
  `load.rs` right after the surface is handed over, the same way
  `cordial_game_activity_init` drives `initializeNativeCode` directly instead
  of waiting for a Java caller that does not exist. Signatures are from the
  shipping APK's dex (`tools/dex_method.py`), not guessed:
  `setState(Lcom/google/androidgamesdk/gametextinput/State;)V`,
  `setSoftKeyboardActive(ZI)V`, `restartInput()V`.
- `GameActivity.setImeEditorInfoFields(III)V`/`setWindowFlags(II)V` — also
  previously unresolved, now real no-op hooks; resolving is the point, per
  `NativeTextBoxInfo`'s own comment on the pending-exception hazard an
  unresolved call carries.
- `android::input::reseed_if_needed` (`crates/cordial-runtime/src/android/
  input.rs`) now prefers `InputConnection.setState`'s text and caret over
  `showKeyboard`'s one-shot byte-array snapshot, once at least one `setState`
  has actually arrived — `setState` is refreshed rather than captured once at
  focus time, and carries a real caret where `showKeyboard`'s array carries
  none.

**Deliberately not done, and why:** reseeding *live*, on every
`ime_state_generation()` change rather than only at the existing
focus-change boundary. `setState` is also how the engine would echo back
whatever Cordial itself just pushed via `pass_text`/`sync_textbox`; treating
every echo as a fresh overwrite mid-keystroke is the same shape of feedback
loop that produced the focus-bounce bug two sections up, and confirming a
live-overwrite version does not reintroduce it needs the interactive test
this change has not yet had (see below).

**Verified:** `setInputConnectionNative` registers cleanly
(`CORDIAL_ANDROID_TRACE=1` shows `InputConnection registered with the
engine`) and a full run reaches `APP_READY (Landing)` with no
`Constructed Unresolved symbol` for `InputConnection` or either `GameActivity`
method, on Wayland. **Not yet verified:** that `setState` actually arrives
once a field is focused and typed into, or that the reseed change makes
characters appear — both need clicking into a real field and typing, which
this session's automated environment could not do (the desktop session was
screen-locked for the remainder of it). Do the interactive test — click a
field on either backend, type, screenshot — before trusting this as the fix
rather than as a well-motivated, resolves-cleanly, unverified-live change.

### Why the text is invisible while you type: Android draws it with a real widget

Established 2026-08-02, and it reframes this whole section. The engine is not
failing to receive the text and there is no message that makes it render.

**The symptom.** Typing into a focused box shows nothing and draws no caret. On
blur the full, correct string appears at once.

**What that rules in.** Cordial sends *nothing* at blur — `nativePassText` is off
by default and `hideKeyboard` only marks the box blurred. So the correct string
that appears on blur can only have arrived through the per-keystroke
`syncTextboxTextAndCursorPosition2` calls. The engine held it the whole time and
withheld the *drawing*, deliberately.

**Why.** On Android the editing-time display is not the engine's job. It belongs
to a real `android.widget.EditText` laid over the GL surface:

```text
Lcom/roblox/client/RbxKeyboard; -> Lq/l; -> Landroid/widget/EditText;
```

Verified from the dex `class_def` superclass chain. `RbxKeyboard` carries
`getCurrentTextBox()J`/`setCurrentTextBox(J)` — the same handle `showKeyboard`
passes — plus `i(NativeTextBoxInfo, String)`, `l(NativeTextBoxInfo)`,
`setManualFocusRelease(Z)`, `onSelectionChanged`, `onKeyPreIme`, `autofill`, an
inner `TextWatcher` and an `OnEditorActionListener`. In
`res/layout/activity_game.xml` it is a sibling of the GL surface's container,
`background=@android:color/transparent`, `visibility=gone`, `match_parent` —
a transparent editor revealed over the surface on demand.

That is what `NativeTextBoxInfo`'s fields are *for*. `x, y, width, height,
fontSize, font, textColor, xAlignment, yAlignment, multiline, textWrapped`
are not IME hints; they are how to style a widget so it looks exactly like the
Roblox box underneath it. Only `textInputType` and `returnKeyType` configure an
IME. And the engine pushes text *out* during editing —
`onLuaTextBoxChangedCallback(String)` and the no-argument
`onLuaTextBoxPropertyChangedCallback()`, whose only sensible response is to
re-read that geometry. A "properties changed" callback is only needed if Java is
displaying the box. **Both are unimplemented in Cordial**
(`docs/analysis/unresolved-java.md` §2c).

So the shadow buffer was never the problem, and deleting it was never going to
help: **the missing piece is a widget, not a message.** Cordial has to draw the
editing text itself, positioned and styled from those 14 fields, which
`NativeTextBoxInfo::init` (`native/android_classes.cpp:220`) currently accepts
and discards.

**There is already an instrument for this and nobody was reading it.**
`FLog::NativeInput` and `FLog::DataModelBindings` are on by default and narrate
the path in Cordial's own engine log, no `flags.json` needed:

```text
onTextBoxFocused: 0x7f366c4d0080
handleTextBoxFocused_AndroidLayer_:
```

in `~/.local/share/cordial/instances/default/data/files/appData/logs/*_Player_*.log`.
Note the engine's own name for it: `_AndroidLayer_`.

### Confirmed 2026-08-03: a focused box does not draw its own text, and the keyboard report is not what decides it

The experiment this section asked for has been run, three times, on the X11
backend with `--profile agent-text`, build `0.3.0-21-gf70ee23-dirty`. It is no
longer "not yet confirmed".

**How it was driven, because that was the blocker for four separate
investigations.** `CORDIAL_SCRIPT` now takes `click:640x308` and `type:abc`
alongside `fullscreen`/`motion-on` — a click and a keystroke through Cordial's
own input path (`input::script_click`/`script_type`), calling the same natives
with the same arguments `window.rs`'s and `wayland.rs`'s own
`dispatch_button`/`dispatch_key` call. Nothing goes near a compositor, so the
rule that forbids `XTestFake*`/`ydotool`/`wlr-virtual-keyboard` is untouched:
Cordial *is* the client and there was never anything to inject into. Every
sentence in this file that ends "that needs a keystroke, which no Wayland-safe
automation here can supply" is now wrong. The whole login-form sequence is one
launch:

```bash
CORDIAL_TRACE_TEXT=1 CORDIAL_INSTR=1 \
CORDIAL_SCRIPT=25:click:640x382,32:click:640x308,34:type:abc,40:click:640x374,48:click:640x308 \
  just client --x11 --run 70
```

`--x11` because the window it makes is an ordinary X11 window, so
`import -window <id>` photographs it. There is no equivalent on this GNOME
Wayland session: `org.gnome.Shell.Screenshot` answers `AccessDenied` and `grim`
is not a thing outside wlroots. That is a limitation of the *observation*, not
of the backend under test — the engine renders into a Vulkan swapchain either
way.

**What it showed**, at 1280x720, clicking Sign In, then the username field:

| t | what happened | what the window showed |
|---|---|---|
| 33 | box focused, empty | placeholder `Username/Email/Phone` |
| 36 | `abc` typed, `sync=true` for each character | **nothing** — no text, no caret |
| 44 | password field clicked, so the username box blurred | **`abc`** |
| 52 | username box clicked again, nothing typed | **nothing** — `abc` gone again |

At t=52 the engine's own trace reads `textbox focused handle=… current=3 bytes`,
so the engine is holding `abc` and has been asked to draw a box it knows
contains it. It draws an empty box. **The existing text vanishes on focus
alone**, which is exactly the condition this section named for the widget
diagnosis holding.

Repeated with `xyz`, and the append case works too: refocusing the box that
already held `xyz` and typing `d` gives `xyzd` on the next blur, so nothing is
being lost — only withheld from the screen while the box has focus.

**The `updateKeyboardSize` theory below is disproved, in the same session, with
the control it asked for.** The run above with `CORDIAL_KEYBOARD_REPORT=1` —
which now sends the corrected `visible=false, x=0, y=720, w=1280, h=0` baseline
at every focus change, the shape the real-Android capture shows — is
pixel-identical at every one of those four moments. The engine was told, truthfully
and in the real client's own wording, that no soft keyboard is up, while a box
had focus and text was arriving, and it still drew nothing.

Two things worth keeping from that run: the corrected report **does not bounce
focus** (the `visible=true` with zero height that did is long gone), and it
changes nothing else either, so `CORDIAL_KEYBOARD_REPORT` stays off by default
for want of a reason rather than for fear of it.

**Where that leaves the Sober counter-evidence below: unverified, and nobody
here has checked it.** The claim is second-hand — Sober shows typed text live —
and the one Sober engine log on this machine
(`~/.var/app/org.vinegarhq.Sober/data/sober/appData/logs/*_Player_*.log`) has
zero hits for `TextBox` in 864 lines, so that session never focused a field and
cannot corroborate anything. If somebody wants to settle it, that log is where
the evidence would be: focus a box in Sober, type, and grep the log for
`handleTextBoxFocused_AndroidLayer_`. If Sober's engine narrates the same
`_AndroidLayer_` handover and still draws the text, the difference is in what
Sober answers, and this section is wrong. Until then the measurement above
stands and the second-hand claim does not.

**What the overlay needs, and it is less than it sounds.** `showKeyboard` already
hands over the box in window pixels, and Cordial already parses it —
`CORDIAL_TRACE_TEXT=1` prints, for the username field of a 1280x720 login form:

```text
textbox spec from showKeyboard x=470 y=297 w=340 h=22 fontSize=16 textColor=0xffd5d7dd
```

Measured against the screenshot, that rectangle is the *text* area inside the
rounded field (which spans roughly x 460..820, y 293..325), in the same
coordinate space the click that focused it used, top-left origin. So the
positioning half of an EditText-equivalent needs no new engine call at all: it
needs a surface over the canvas — a `wl_subsurface` on Wayland — a font, and the
buffer `input::text_buffer_snapshot` already keeps. Not attempted here, because
a half-built one is worse than none.

**A thing observed and not explained, so nobody reads past it:** the placeholder
label hides when the text arrives in two of the three runs and stays visible in
the third, with identical scripts. The placeholder is a separate label the Lua UI
shows while `Text` is empty, so hiding it means the property really did change;
staying visible in one run means either it did not, or the frame was stale.
Presents sit at exactly 1.0/s here — the idle throttle (§1d) — and **typing does
not lift it**, while a click does (6.9/s), so a frame is up to a second old and
the engine is not redrawing on text arriving. That is a lead for whoever chases
`onLuaTextBoxChangedCallback`, and it is not evidence of anything on its own.

**Counter-evidence, raised 2026-08-02, and unverified — see above.** Sober shows
typed text live, as you type — and Sober's engine process links raw EGL/GLESv2
with its own Wayland client and no toolkit at all. Its GTK4/libadwaita/WebKitGTK
usage is in a separate binary for web views, so there is nothing over its
surface that could be drawing that text. If the engine could only ever be drawn
into by an overlay editor, Sober could not do what it demonstrably does.

The mechanism that suggested, and it is the one the measurement above kills: on
Android, touching a field does not raise the soft keyboard when a hardware
keyboard is attached, and with no soft keyboard up the engine would draw its own
text and its own caret. What tells it which case it is in is `updateKeyboardSize`
— which Cordial had never once reported correctly *while someone typed*. It has
now, and the screen did not change.

### Ctrl+V is bound, and `CORDIAL_TRACE_TEXT` no longer prints what you typed

Two changes on the same path, 2026-08-03.

**`CORDIAL_TRACE_TEXT=1` used to put the focused field's contents on the
terminal** — once per keystroke from `pass_text`, and again per key from each
backend's own trace line. The field anyone debugging this reaches for is
Roblox's password box. It now prints a byte and character count and the caret,
which answers every question the switch exists for, since the bug is that
characters do not *paint* rather than that the wrong ones arrive.
`CORDIAL_TRACE_TEXT_SHOW_PASSWORDS=1` prints the text, and is named so that
nobody turns it on without deciding to. Verified in a live run: a paste of the
developer's own clipboard through the focused box logged
`text -> <26 bytes, 26 chars> caret=26` and nothing else.

**Ctrl+V now pastes into the focused box**, in both backends, through
`clipboard::paste_into_engine`. Note the shape, because it explains why there is
no engine call to hunt for: on Android the `EditText` over the GL surface
handles the paste itself and the engine only ever sees text arrive through
`gametextinput`, so a paste is an insert on the same path a keystroke takes.
Cordial is that editor. `input::is_paste_shortcut` is unit-tested (Ctrl+Shift+V
is deliberately *not* paste — it means "without formatting" everywhere else),
and `paste_into_engine` was measured by the work that added it. **`INFERRED`:**
the two centimetres of wiring between them have never been crossed by a real
Ctrl+V, because a real keystroke needs the developer's own keyboard and the
scripted seam calls `paste_into_engine` directly. On the X11 backend it will
fail honestly — GTK has no display open there, which is where the clipboard
lives, and the trace says so.

### The capture does not cover text entry. Do not grep it for this

Checked exhaustively on 2026-08-02, because the repo's one rule sends everyone
here first. `docs/traces/` holds one capture — `waydroid-roblox-startup.log.gz`,
2432 lines — and the session that produced it **never touched a TextBox**. It
launches `ActivitySplash`, reaches the Lua home screen, and is backgrounded
without interaction.

Hits across the whole capture, case-insensitive: `syncTextboxTextAndCursorPosition`
0, `nativePassText` 0, `nativePassKeyEvent` 0, `nativeGetTextBoxInfo` 0,
`nativeReturnPressed` 0, `NativeTextBoxInfo` 0, `TextBox` 0, `InputConnection` 0,
`InputMethodManager` 0, `restartInput` 0, `setImeEditorInfoFields` 0,
`setSoftKeyboardActive` 0. `showKeyboard`'s two hits are both inside the flag
name `EnableTextInputRestoreOnShowKeyboard`, not a call.

So for text entry the trace is not a lookup, and the rule's protection does not
apply — which is exactly when this project has historically gone wrong. **The fix
is another capture, not another theory:** the same `adb logcat` procedure in
`docs/traces/README.md`, driven into the home screen's search box (reachable
without a login), typing a few characters and blurring. `rbx.glview.layout`
already logs at verbosity V, so a keyboard-visible `onUpdateKeyboardSize()` would
appear the moment the IME opens.

**What the capture does establish**, twice, at surface bring-up (lines 1113 and
1263, immediately after the `SurfaceView` resize and before `surfaceCreated`):

```text
rbx.glview.layout: [a.e()-51]: onUpdateKeyboardSize() v:false x:0 y:999 w:2491 h:0
```

That confirms `updateKeyboardSize(Z,I,I,I,I)` is `(visible, x, y, width, height)`,
and that the real client's keyboard-hidden baseline is `visible=false` with a
**real** rectangle — full UI width, zero height, at the bottom. Not an empty one.
`INFERRED`, and flagged by the agent that found it: that line is the app's own
Java-side layout callback, and a 1:1 correspondence with the JNI call was not
established.

The capture never shows `updateKeyboardSize` with `visible=true`, so it cannot
say what rectangle the engine gets when an IME is actually up.

## 1a. The Wayland backend was blank — two independent bugs, both fixed

`CORDIAL_WAYLAND=1` produced a window present in the dock and alt-tab, titled
correctly, completely blank on screen, for the entire time this project has
had a Wayland backend. Two unrelated bugs, both real, both now fixed in
`crates/cordial-runtime/src/android/vulkan.rs` and
`crates/cordial-runtime/src/android/wayland.rs`. Neither was found by reading
FLog — see the note in §2a above about why FLog is not useful for this
question — both were found by instrumenting the actual Vulkan/Wayland calls
and comparing X11 (works) against Wayland (did not) with real numbers.

**Bug 1: `vkGetPhysicalDeviceSurfaceCapabilitiesKHR`'s `currentExtent` was
never patched for Wayland, and the engine cannot handle the value Wayland
sends.** `VK_KHR_wayland_surface` reports `currentExtent` as
`(0xFFFFFFFF, 0xFFFFFFFF)` — the spec's own "the client picks the size, not
the platform" sentinel, because unlike an X11 window or a real Android
`ANativeWindow`, a Wayland surface has no size of its own until a buffer is
attached. Confirmed directly: `vkGetPhysicalDeviceSurfaceCapabilitiesKHR ->
0, ... currentExtent=4294967295x4294967295` on Wayland,
`currentExtent=1280x720` on X11, for the identical call. The engine's own
FLog explains what it does with that: `Vulkan: skipping framebuffer creation,
invalid currentExtent -1x-1`, repeated every frame, forever — its surface
code was written against Android's always-a-real-size `VkSurfaceKHR` and has
no path for "you choose". Confirmed with a second, independent counter:
`vkCreateSwapchainKHR` and `vkAcquireNextImageKHR` were called **zero** times
on Wayland for a whole run that reached `APP_READY (Landing)`, against one
`vkCreateSwapchainKHR` and 653 `vkAcquireNextImageKHR` calls on X11 in the
same window — the engine never even attempted to create a swapchain.

The `Invalid currentExtent -1x-1` line is a trap worth naming explicitly: it
also fires continuously on X11, which renders correctly, from an unrelated,
harmless periodic check elsewhere in the engine. Trusting that line alone
without comparing the two backends' actual `vkCreateSwapchainKHR`/
`vkQueuePresentKHR` call counts would have (and briefly did) point at the
wrong conclusion.

Fix: `vk_get_physical_device_surface_capabilities_khr` in `vulkan.rs`
intercepts the call on the Wayland backend only and, when `currentExtent` is
the undefined sentinel, replaces it with the Wayland window's own current
size — the same "report what an Android surface would report" substitution
this file already makes for the surface identity itself
(`vkCreateAndroidSurfaceKHR -> vkCreateWaylandSurfaceKHR`).

**Bug 2: a second `wl_proxy_add_listener` call on `xdg_surface` silently
failed, leaving a dangling stack pointer registered, which segfaulted the
moment the fixed Vulkan path tried to resize.** Once bug 1 was fixed and the
engine started really presenting, the process reliably segfaulted a few
frames later — `wl_closure_invoke` (inside `libwayland-client`) jumping to
address `0xe0` via `libffi`'s `ffi_call`, i.e. calling through a garbage
function pointer. `open()` in `wayland.rs` used to register a temporary,
stack-local `XdgSurfaceListener` for the *first* `xdg_surface.configure`
(before `WaylandWindow` exists for a steady-state listener to reach via
`current()`), then called `wl_proxy_add_listener` a second time to swap in
the real, `'static` listener once construction finished. Logging that second
call's return value directly showed `-1` — `wl_proxy_add_listener` refuses a
second registration on a proxy that already has one and changes nothing —
so the dangling stack listener (a local in `open()`, which had long since
returned and had its stack frame reused many times over) stayed registered
for the whole session. The *first* subsequent `xdg_surface.configure` — which
never arrived before bug 1 was fixed, because nothing had ever made the
window worth reconfiguring — read whatever unrelated bytes now occupied that
stack slot as a function pointer and jumped to them.

Fix: one `XdgSurfaceListener`, registered once, for the proxy's whole
lifetime. It writes the initial serial into a small static
(`INITIAL_XDG_SURFACE_SERIAL`) when `current()` is still `None`, instead of a
second listener swap. See `xdg_surface_configure`'s own comment in
`wayland.rs` for the full trace that found this.

**While debugging bug 2, two more listener structs were found undersized the
same way** — `PointerListener` was missing `frame`/`axis_source`/
`axis_stop`/`axis_discrete`/`axis_value120`/`axis_relative_direction`
(`wl_pointer` v5/v5/v5/v5/v8/v9) and `KeyboardListener` was missing
`repeat_info` (`wl_keyboard` v4). Both fixed with no-op handlers for the same
reason `XDG_TOPLEVEL_EVENTS` needed `configure_bounds`/`wm_capabilities`
added: `wl_pointer_interface`/`wl_keyboard_interface` are `dlsym`'d from the
host's real `libwayland-client.so`, so their `event_count` is whatever the
host's library version actually declares, not whatever this file happens to
have a listener field for, and a wire event past the end of a too-short
listener array is exactly this crash. Neither has actually been observed to
fire in a captured run yet — they are defensive, following the same
protocol-version-vs-listener-size reasoning that explained bug 2, not each
individually confirmed the way bug 2 was.

**Verified, not inferred:** `vkQueuePresentKHR` went from 0 to 663 calls on
Wayland (668 on X11, same window, same run length), `vkCreateSwapchainKHR`
now succeeds, and the process completes a full run and reaches
`APP_READY (Landing)` repeatedly with no crash — checked across several
consecutive launches, not once. **Not yet verified with a screenshot.** The
desktop session locked partway through this work (screen-idle timeout, not
caused by anything here) and did not unlock again before this session ended.
Everything above is measured through the engine's and Mesa's own return
values and call counts, which is real evidence that frames are being
produced and handed to the compositor — it is not the same claim as "a
screenshot shows Roblox on screen", and the two should not be conflated. Take
that screenshot (`docs/NEXT.md`'s own note on GNOME's `org.gnome.Shell.
Screenshot`/portal screenshot mechanisms working where X11 tools cannot
capture a native Wayland surface still applies) before treating this as
fully closed rather than "presentation demonstrably works; pixels on screen
not yet independently confirmed".

**Do not re-run:** blaming `Invalid currentExtent -1x-1` in FLog by itself —
see above, it is not diagnostic on its own. Do not re-add the "swap listener
after `WINDOW` exists" pattern for `xdg_surface` — see bug 2.

### 1a (cont.) The window is a libadwaita window now, and the engine sits inside it

Landed 2026-08-02. The engine's `wl_surface` was its own `xdg_toplevel`, which
is why there was no titlebar: the canvas *was* the window. It is now a
`wl_subsurface` of a GTK4/libadwaita toplevel built by
`cordial_shell::host_window` — the same definition the shell binary uses — and
positioned over that window's content area.

**Verified by running:** three consecutive 25-second launches reach
`APP_READY (Landing)` with 547, 548 and 550 `vkQueuePresentKHR` calls, no crash;
a screenshot shows the libadwaita header bar above Roblox's landing page, which
the same session's pre-change binary does not have. `WAYLAND_DEBUG=1` shows
`wl_subcompositor.get_subsurface`, `wl_subsurface.set_desync`,
`set_position(25, 71)` and `xdg_toplevel.set_app_id("Cordial")` on the wire —
the app_id had previously only ever been checked by a unit test against the
desktop entry, never observed.

**Resize was verified, and not by dragging.** A temporary local patch (not
committed) called `gtk_window_set_default_size` from a timer at 10s and 16s into
a run, 1280x721 -> 700x460 -> 1500x900. Both took effect with the engine live:
screenshots show Roblox's landing page re-laid out at each size under the header
bar, the run reached `Landing` and presented 553 frames, and nothing crashed.
That exercises the same path a compositor-driven resize takes — content
allocation changes, `sync_canvas_geometry` moves and resizes the subsurface,
`surface_resized` reaches the engine, and the engine rebuilds the swapchain
itself — with a
different trigger. **Not Mesa**, as this sentence said until 2026-08-25: nothing
in Cordial creates or destroys a swapchain. The engine polls
`vkGetPhysicalDeviceSurfaceCapabilitiesKHR`, Cordial substitutes the extent
`apply_resize` has just published for Mesa's `0xFFFFFFFF` sentinel, and the
engine calls `vkCreateSwapchainKHR` on its own. The distinction matters because
"the swapchain was recreated" reads like something done *to* the engine, and a
whole afternoon went on looking for the object Cordial had pulled out from under
it. There is no such object. Dragging the window edge by hand is still untested,
because there is no Wayland-safe way to do it from automation.

**Experiment 1 below is answered — no, it does not — and it did not need a
human after all.** `CORDIAL_SCRIPT` clicks and types now, through Cordial's own
input path and nowhere near a compositor; §1's 2026-08-03 correction has the
measurement and the command. What is left here needs a click only in the sense
that somebody has to be watching a Wayland session while it happens.

```bash
# 1. ANSWERED, 2026-08-03: no. See §1. Kept for the shape of the command.
#    Add CORDIAL_SCRIPT=…click:…,…type:… and it runs itself.
CORDIAL_KEYBOARD_REPORT=1 CORDIAL_TRACE_TEXT=1 CORDIAL_WAYLAND=1 \
  ./target/release/cordial-run --lib-dir <lib> --apk <apk> \
  --host-libc --game-activity --run 120

# 2. Does the zwp_text_input_v3 "has no event 8" freeze still happen, and on
#    which object? Click into a field; WAYLAND_DEBUG prints every event with
#    its object id, so the id that receives opcode 8 can be matched against
#    what created it earlier in the same log.
WAYLAND_DEBUG=1 CORDIAL_WAYLAND=1 ./target/release/cordial-run ... 2>&1 \
  | tee ~/.cache/ti.log; grep -n "text_input\|no event" ~/.cache/ti.log
```

**Experiment 2 is answered, and the answer did not need a click** — see
§1c below. Event 8 is `preedit_hint`, added in `zwp_text_input_v3` **version
2**, which this file's own `bind` had always asked for while the hand-written
table beside it described version 1. The table is fixed. The experiment is
still worth running once as confirmation, because nobody has yet seen mutter
send opcode 6, 7 or 8 to Cordial — but it is no longer the only way to
diagnose it.

**Three things worth knowing before touching this code.**

`wl_subsurface.set_desync` is mandatory — a subsurface starts synchronised and
its commits then wait for the parent's, and GTK only commits when it draws, so
an idle window would show one engine frame per accidental repaint.

GTK will not open a Wayland display if `GDK_BACKEND` names something else, even
after `gdk_set_allowed_backends("wayland")`; the two are separate filters and
their intersection was empty. It fails silently — `gtk_init_check` returns false
and prints nothing. This session's GNOME desktop exports `GDK_BACKEND=x11`, so
this is not a hypothetical.

Cordial's `wl_pointer` is now a second pointer object on the same seat as GDK's,
so it sees enters and clicks aimed at the header bar. `POINTER_ON_CANVAS` filters
them; without it the engine reacts to a click on the close button and the cursor
vanishes over the titlebar.

**`wl_keyboard` focus is unchanged in effect and stricter in code.** The fix that
stopped Cordial seeing keystrokes typed into other windows — `KEYBOARD_FOCUSED`,
set on `enter` and cleared on `leave`, checked before any key is processed —
still gates every key. `keyboard_enter` now additionally requires the entered
surface to be *this window's* toplevel rather than any surface of the client,
which only narrows what counts as focus. The behaviour was not re-tested against
a real keystroke, because that needs a human; the code path is a strict subset of
the one that was tested.

**Now that Wayland presents frames, [ADR-011](adr/ADR-011-wayland-and-libadwaita.md)'s
removal trigger is close but not met.** The ADR is explicit that `window.rs`
and the X11 backend are deleted "when the Wayland backend can reach sign-in",
not when it renders — and sign-in is still blocked on §2 below, unrelated to
anything in this section. X11 stays in the tree, and stays load-bearing as
the control, until that condition is actually met.

## 1b. The canvas lags the window for a frame after a resize

Reported and reproduced visually: for a split second during a resize the GTK
window is already the new size while the engine's canvas is still the old one,
leaving a black band where nothing has been drawn.

The cause is `wl_subsurface.set_desync` at `wayland.rs:895`, and it is not a
mistake — a subsurface starts *synchronised*, and desync is what lets the engine
present at its own rate instead of waiting for GTK to commit. The cost is that a
resize stops being atomic: GTK resizes the toplevel and commits immediately,
while the canvas keeps its old buffer until the engine happens to render again.

The fix is not to abandon desync. It is to be **synchronised only while
resizing** — `set_sync` on the `xdg_toplevel` configure, `set_desync` once the
engine has presented at the new size — so a resize commits atomically and normal
rendering stays independent. `INFERRED`: this is the standard remedy for a
toolkit window hosting an independently-driven surface and has not been tried
here.

Two things not to reach for. Delaying `ack_configure` until the engine has a
matching buffer makes GTK stall on the engine's frame rate. `wp_viewporter`
scaling the stale buffer hides the seam by showing a stretched old frame, which
is a different wrong picture rather than none.

## 1c. A Wayland protocol error killed a signed-in session. Reproduced logged out, and the timer turns out to be Mesa's

Reported once, on a real signed-in session, minutes after reaching the home
page:

```text
[roblox] datamodel notification: LUA_HOME_PAGE_LOADED
[roblox] datamodel notification: HOME_PAGE_INTERACTIVE
[cookies] periodic: saved 4 domain(s), 5032 bytes
Gdk-Message: 14:10:43.968: Error 71 (Protocol error) dispatching to Wayland display.
```

**Correction: it does happen logged out, and the unmuting worked.** The
paragraph that used to stand here said reproducing it needed a signed-in
account. It does not. One run in eight, logged out, 22-second runs on this
compositor, immediately after `app ready: Landing`:

```text
[roblox] datamodel notification: APP_READY Landing
[roblox] app ready: Landing
[stub] ZSTD_trace_compress_begin
[wayland] wp_commit_timer_v1#105: error 1: Commit already has timestamp

Gdk-Message: 16:04:10.242: Error 71 (Protocol error) dispatching to Wayland display.
```

So the object and the reason are now on the record: `wp_commit_timer_v1`
error 1, which is `commit_timer_v1.error.timestamp_exists` — a second
`set_timestamp` on a surface that already had one before its next commit.

Five earlier logged-out runs (two at 120s and 180s before any change, three at
180s after) reached `APP_READY (Landing)` with no protocol error, which is
consistent with roughly one in eight rather than with "does not happen".

### The timer is Mesa's, on Cordial's own canvas. It is not GTK's, and `queue_commit` is not a commit

**Correction, and this paragraph replaces the one it grew out of.** What used to
stand here read the error as "Cordial commits the *parent* toplevel itself
(`host.queue_commit`), and GTK also drives that same surface through its frame
clock. Two clients of one surface." It also called `wp_commit_timing_v1` "GTK's
frame-timing protocol, on a surface GTK owns". **Every clause of that is wrong.**
`WAYLAND_DEBUG=1` on a run that reproduced the error says so by object id — one
run in thirteen, 20-second runs, logged out. The log is the whole answer.

The surfaces, from that log:

```text
-> wl_compositor#4.create_surface(new id wl_surface#47)      GTK's toplevel
-> wl_compositor#108.create_surface(new id wl_surface#76)    the engine's canvas
-> wl_subcompositor#104.get_subsurface(new id wl_subsurface#75, wl_surface#76, wl_surface#47)
```

and the object the compositor killed the connection over:

```text
{mesa vk display queue} -> wp_commit_timing_manager_v1#63.get_timer(new id wp_commit_timer_v1#105, wl_surface#76)
{mesa vk display queue} -> wp_commit_timer_v1#105.set_timestamp(0, 197598, 391623000)
wl_display#1.error(wp_commit_timer_v1#105, 1, "Commit already has timestamp")
```

The timer belongs to **Mesa's Vulkan WSI**, and it is attached to
**`wl_surface#76` — the engine's own canvas subsurface**, not to GTK's toplevel.
GDK creates no `wp_commit_timer_v1` anywhere in the log; every `set_timestamp`
on #105 is Mesa's, one per present.

`HostWindow::queue_commit` is `gtk_widget_queue_draw`. It does not emit
`wl_surface.commit` and never did — it asks GTK to repaint, and GTK's own commit
is what latches `set_position`. Cordial sends no request whatever on GTK's
surface. Counted over a 45-second run: `wl_subsurface.set_position` fired **0**
times and `queue_commit` **0** times after startup, because `sync_canvas_geometry`
acts only when the content rectangle moves and on an untouched window it never
does.

What the wire does show is Mesa issuing, for every present of #76:

```text
-> wl_surface#76.attach(wl_buffer#122, 0, 0)
-> wl_surface#76.damage(0, 0, 2147483647, 2147483647)
-> wp_commit_timer_v1#105.set_timestamp(0, 197598, 391623000)
-> wp_fifo_v1#106.set_barrier()
-> wp_fifo_v1#106.wait_barrier()
-> wl_surface#76.commit()
-> wp_fifo_v1#106.wait_barrier()
-> wl_surface#76.commit()
```

two commits per present, driven from two of Mesa's event queues at once —
`{mesa vk display queue}` and `{mesa vk surface 76 swapchain 1 queue}` interleave
throughout the trace. Immediately before the error a present was issued **1 ms**
after the previous one against an otherwise steady 20 ms cadence, and
`wl_buffer#122` was re-attached before its `release` had been dispatched. Two
threads inside one swapchain's present path is what that looks like, and a second
`set_timestamp` before the intervening commit is what the compositor refused.

**Not established: whether Cordial provokes it.** The one thing Cordial does that
an ordinary Vulkan client does not is hand Mesa a `wl_display` that GDK also owns
— and there are *two* Vulkan swapchains on that connection, because GTK renders
through Mesa's WSI too and took commit timers on `wl_surface#47` earlier in the
same log. Nothing was measured either way.

**A rate to plan against, so nobody reads a clean run as a fix.** Sixteen
25-second baseline runs gave one occurrence; thirteen 20-second `WAYLAND_DEBUG`
runs gave one. In the reproducing log the error fired ~3.6 s into a presenting
burst, so the exposure that counts is **frames presented, not seconds elapsed** —
on the order of one in ten thousand presents. A 240-second run with continuous
input (~6,000 presents) came back clean, and that is **not** evidence of
anything.

**Do not re-run:** the "two committers on one surface" theory, or a control with
`queue_commit` suppressed. Both are answered above, by object id.

### Why that line was the whole of the evidence, and why it will not be again

Errno 71 is `EPROTO`, and **only** a compositor-sent `wl_display.error`
produces it. Measured, by asking mutter to bind a global it never advertised:

```text
wl_registry#2: error 0: global wl_compositor (999999) is unavailable
roundtrip=-1 wl_display_get_error=71 (Protocol error) errno=71 (Protocol error)
```

So the compositor did name the object and the reason. **GDK then threw it
away.** GTK4 calls `wl_log_set_handler_client` with a handler that logs at
`G_LOG_LEVEL_DEBUG`, and debug is dropped unless `G_MESSAGES_DEBUG` names the
domain — so libwayland's one useful line is discarded about 50ms before GDK
prints its errno and calls `_exit(1)`. Confirmed by planting the same
deliberate bad `bind` inside `open()` and launching the real client: the entire
output was one `Gdk-Message` line, byte-for-byte the shape of the report above.
With `G_MESSAGES_DEBUG=all`, the same run also printed

```text
(Cordial:96812): Gdk-DEBUG: wl_registry#107: error 0: global wl_compositor (999999) is unavailable
```

`cordial_shell::host_window::unmute_waylands_own_errors` now installs a
`Gdk`-domain handler that re-emits those lines as `[wayland] ...` regardless of
`G_MESSAGES_DEBUG`, filtered so the ~122 portal-settings debug lines GDK also
emits per launch stay out of the way. Verified with the deliberate error, three
consecutive launches, against the same three launches of the pre-change binary
that printed nothing.

**So the next occurrence is self-diagnosing.** Whoever hits it again should
paste the `[wayland] <interface>#<id>: error <code>: <reason>` line — that
single line names the offending object and the compositor's own words for what
was wrong with it, which is the whole answer.

### What was found instead, and it is a real bug: the text-input table described the wrong protocol version

Chasing the above through `WAYLAND_DEBUG=1` turned up a different, definite
defect, and **corrects what §1a's module doc said about it.**

`wayland.rs` binds `zwp_text_input_manager_v3` at version 2 — measured on the
wire, `wl_registry#107.bind(26, "zwp_text_input_manager_v3", 2, ...)`, because
GNOME 50's mutter advertises 2. A `zwp_text_input_v3` created by a v2 manager
**is** a v2 object; the version a client passes to `wl_proxy_marshal_flags`
does not change what the compositor believes it may send. And version 2 adds
three events to version 1's six: `action` (6), `language` (7) and
`preedit_hint` (8).

The hand-written table in `wayland.rs` declared six. **Event 8 is
`preedit_hint`** — checked against `wayland-scanner`'s own generated table for
the shipped XML, which matches the corrected table name-for-name and
signature-for-signature. The old comment's explanation, that "event 8 exists in
`zwp_text_input_v2`", named a different protocol and was wrong.

**This is not the EPROTO above, and conflating them would send the next person
the wrong way.** An opcode past the end of a client's own table is refused
*inside libwayland*, not by the compositor; it leaves `errno` at whatever it
happened to be. Reproduced standalone against this compositor by binding
`wl_seat` at version 8 behind deliberately short and complete tables, five
times each in one session:

```text
SHORT  bound wl_seat v8, table declares 1 event(s): roundtrip=-1  wl_display_get_error=11 (Resource temporarily unavailable)
FULL   bound wl_seat v8, table declares 2 event(s): roundtrip= 4  wl_display_get_error=0 (none)

5/5 SHORT runs killed the display; 5/5 FULL runs were clean.
```

11, not 71 — and the whole display dies, every client on the connection
included, which is the freeze §1a recorded rather than a crash.

The fix is the complete v2 table, three no-op listener slots, and `bind` taking
its version from `TEXT_INPUT_MANAGER_INTERFACE.version` so the request and the
table cannot drift apart again. Two unit tests pin it.

**Still unverified:** that mutter ever actually sends opcode 6, 7 or 8 to
Cordial. Those need an input method composing into a focused field, which needs
a click. What *is* established is that the object is live all session — the
trace shows `zwp_text_input_v3#71.enter(wl_surface#47)` arriving as soon as the
toplevel takes keyboard focus, with no `enable` sent and no field clicked — so
there is no window in a session where a v2 event would be harmless.

### Ruled out as the cause of the EPROTO, so nobody re-runs them

- **Short listener arrays on `wl_pointer`/`wl_keyboard`/`wl_registry`.** Dumped
  the host `libwayland-client.so`'s own tables directly: `wl_pointer` declares
  11 events, `wl_keyboard` 6, `wl_registry` 2, and the structs in `wayland.rs`
  have exactly 11, 6 and 2 fields. §1a's defensive padding was correct.
- **§1a's padding being load-bearing.** It is not, on this compositor:
  `wl_seat` is bound at version 1, so mutter never sends `frame` or the axis
  events at all — zero `wl_pointer#70.frame` in a full 120s run. Keep the
  slots; do not cite them as tested.
- **Anything reachable without signing in.** Five runs, no error.

### One thing fixed on the way, of the same family and never observed to fire

`open()` registered the `wl_registry` listener with a pointer to a
`Globals` **local**, and the registry proxy is never destroyed. Any global
appearing later in the session — a monitor hotplug, a seat — would run
`registry_global` against `open()`'s long-dead stack frame. That is §1a bug 2
with a write instead of a call. Now `Box::leak`ed. Not observed to fire; fixed
because it is one of the two shapes this file has already been bitten by.

## 1d. The frame rate. `vkQueuePresentKHR` over a fixed window measures the engine's idle throttle, not the frame rate

**This corrects the metric the rest of this file uses**, including §1a's own
"547, 548 and 550 over three consecutive 25-second runs" and the 1286-1625 over
120-180 s recorded elsewhere. Those numbers are real and they are not frame
rates. Sampled per second instead of totalled, the curve is:

```text
[instr] t=  4.0s presents/s=  59.5
[instr] t= 13.1s presents/s=  60.0
[instr] t= 16.3s presents/s=   1.0
[instr] t= 31.5s presents/s=   1.0     ... and 1.0 for the rest of the run
```

About 60 per second for roughly the first thirteen seconds, and then **exactly
1.0 per second**, for as long as the run lasts. Every historical total is that
curve integrated: 60x13 + 1x12 is 792, 50x11 + 1x14 is 564, and the 526-658
spread across sixteen 25-second baseline runs is the burst ending a second or
two earlier or later.

**The cause is that nothing is happening.** Deliver pointer motion through
Cordial's own input path — `input::deliver_touch` plus `input::pass_mouse_move`,
which "Debugging facts that cost real time" below already permits because no
compositor is involved — and the rate holds at 50-60 per second for a whole
240-second run with no collapse at all. Turn the motion off mid-run and it drops
to 1.0 within two seconds; turn it back on and it is at 50 within one. Both
directions, twice, in one run.

**The control that matters: it is not the Wayland backend.** The identical
binary on X11, no input, same session, shows the same 60-then-1.0 collapse at the
same point in the run — three times on each backend. So this is the engine's own
idle behaviour on the app shell, not `wayland.rs`, not the subsurface, and not
anything about commits.

**Ruled out:** that the engine thinks it lost the window.
`onWindowFocusChangedNative(true)` re-sent 25 s into a run, after the collapse,
returns `Ok(Some(()))` and changes nothing — still 1.0 per second. Do not spend
another session on the focus native; `native/game_activity.cpp` already sends it
once at surface handoff and that call is doing its job.

**What this does and does not say about 45 fps against Sober's 70.** Measured
here, with continuous input:

| | presents/s |
|---|---|
| windowed, 1280x721 | 49-60 |
| fullscreen, 3440x1394 | 49.4 mean over 26 samples |
| idle, any size | 1.0 |

**The rate is the refresh rate of the output the window is on**, and quadrupling
the pixel count does not move it: this desk has a 1920x1200@60.002 panel and a
3440x1440@49.998 monitor, and the numbers above are those two refresh rates. So
the engine is not GPU-bound here at all — it is hard vsync-locked, because Mesa
is using `wp_fifo_v1` on this surface, visible on the wire as
`set_barrier`/`wait_barrier` around every commit.

A reported 70 is *above* both of these refresh rates, so whatever Sober is doing
is not FIFO-throttled. That is a swapchain present-mode difference; it lives in
`vulkan.rs`, which this work did not touch. **Nothing here reproduces or explains
a 45.** With input the engine sits exactly on refresh at both sizes tried. The
owner's 45 is presumably inside an experience, where there is real scene load and
the answer may be entirely different; that was never measured.

**How to measure it next time.** Per-second deltas of
`android::glcount::QUEUE_PRESENT`, with input being delivered, and say which
resolution. A total over a fixed window with an idle app shell answers a
different question than the one being asked.

**Unexplained and left alone:** `ALooper_pollOnce` is called about **9.5 million
times per second**, constantly, on both backends. Cordial's own pump loop
accounts for 20-60 of those per second, counted separately in the same runs, so
the rest is an engine thread spinning. It costs a core and it was not
investigated.

## 1d(ii). The present mode was the ceiling. Asking for MAILBOX takes the landing page from ~36 to a flat 60

§1d ended by saying the difference against Sober "is a swapchain present-mode
difference; it lives in `vulkan.rs`, which this work did not touch". It does, and
this is that work.

**Cordial never requested a present mode at all.** Nothing in the tree mentioned
one, so `VkSwapchainCreateInfoKHR::presentMode` was whatever the engine put
there, and the engine puts `VK_PRESENT_MODE_FIFO_KHR` — the only mode the
specification guarantees exists, and a vsync lock.
`vulkan.rs` now interposes `vkCreateSwapchainKHR` and substitutes
`VK_PRESENT_MODE_MAILBOX_KHR` when the driver advertises it, falling back to
whatever the engine asked for when it does not.

**Measured, with pointer motion delivered in-process at 100 Hz for the whole of
every run**, `--run 120`, windowed at 1280x721 on Wayland, engine at the logged-out
landing page, GameMode registered in all four runs. Build
`v0.2.0-3-g1e1318a-dirty`. Outputs on this desk: eDP-1 1920x1200 at **59.88 Hz**
and HDMI-1 3440x1440 at **49.96 Hz** (`xrandr`). Per-10-second present rate, the
last 60 s of each run once it had settled:

```text
                          per-10s samples from t=30s on            120s total
run 1  off  (FIFO)    41.1 37.4 37.5 37.5 37.5 37.5 35.3 35.0 35.0 36.1   4678
run 3  off  (FIFO)    49.9 49.8 49.7 49.8 49.6 49.9 49.9 49.8 49.8 49.9   5886
run 2  auto (MAILBOX) 60.0 60.0 60.0 60.0 60.0 60.0 60.0 60.0 60.0 60.0   7091
run 4  auto (MAILBOX) 60.0 60.0 60.0 60.1 60.0 60.0 60.0 60.0 60.0 60.0   7091
```

The two MAILBOX runs returned the same 120-second total to the present: 7091.

The control is the same binary in the same session with the substitution turned
off, which is the only thing that makes this a result rather than a number. Both
conditions were run twice, alternating, and GameMode was registered in all four.

**FIFO is variable, MAILBOX is not.** The two controls disagree with each other —
one settled on the 49.96 Hz refresh, the other on 35-37.5, and nothing was
deliberately changed between them. Both are at or below refresh, which is what
FIFO enforces. Every MAILBOX sample in two runs is 60.0 or 60.1. So the honest
statement is a range against a constant: **FIFO 35-50, MAILBOX a flat 60**, and
the floor moved further than the ceiling did.

**This rules out "the engine simply cannot go faster."** The same engine, same
1280x721, same session, goes from 35 to 60.0 on nothing but the present mode. The
FIFO figure was never the engine running out of work.

**One correction to §1d above.** It says the windowed rate "is the refresh rate of
the output the window is on" and gives 49-60 for 1280x721. Run 3 reproduces that
exactly; run 1 does not, sitting at 35-37.5 for six consecutive samples, below
both of this desk's refresh rates. So the claim holds sometimes and is not the
whole story. What produces 37.5 on a 49.96 Hz output was not chased, and the fix
does not depend on the answer.

**The 60.0 is almost certainly the engine's own cap and not a new ceiling.** It
is 600 presents per 10 s, repeated, to the sample. Do not read it as "MAILBOX
gives 60"; read it as "MAILBOX stops the display holding the engine below what it
was already willing to do". A machine whose engine target is higher, or an
experience with real scene load, will not necessarily see this shape.

**The driver here advertises MAILBOX and FIFO and *not* IMMEDIATE**, printed by
the substitution itself. That is the argument for asking rather than assuming:
a client that had been written to force IMMEDIATE would have had to fall back on
this very common Intel/Mesa configuration.

`CORDIAL_PRESENT_MODE=off` is the documented control and stays supported.
`auto` (the default) prefers MAILBOX only — not IMMEDIATE, which also uncaps the
rate but tears.

### Reproducing this

The 100 Hz pointer motion came from a probe thread calling
`input::pass_mouse_move` directly, added to `load.rs` for the measurement and
**removed again before this landed** — `grep -rn perf_probe` finds nothing. It is
half a screen of code to put back and nothing here depends on it being in the
tree. Without it, keep a real pointer moving over the canvas for the whole run,
which drives the same path:

```bash
XDG_DATA_HOME=~/.cache/cordial-perf CORDIAL_COUNT_GL=1 \
  CORDIAL_PRESENT_MODE=off  just client --run 120     # control
XDG_DATA_HOME=~/.cache/cordial-perf CORDIAL_COUNT_GL=1 \
  CORDIAL_PRESENT_MODE=auto just client --run 120     # MAILBOX
```

`vkQueuePresentKHR` in the report at the end, divided by 120, is the rate — but
**only if the pointer was moving the whole time**. Stop moving it and you are
measuring the idle throttle again, which is the trap §1d exists to describe.

## 1d(iii). GameMode is requested, and MangoHUD is a setting that knows when it is absent

Two smaller pieces landed with the present mode.

**Feral GameMode**, in `load.rs`. A D-Bus request rather than a wrapper: nothing
is linked and `gamemoderun` is not involved. `RegisterGame(i pid)` on
`com.feralinteractive.GameMode` before the engine loads,
`UnregisterGame` before `_exit`. On by default, which is what Sober does;
`CORDIAL_GAMEMODE=0` is the off switch and the control. All three paths were run
on this machine and printed:

```text
[gamemode] registered pid 605422: performance governor, raised priority, ...
[gamemode] off (CORDIAL_GAMEMODE=0)
[gamemode] not available, continuing without it: no session bus
```

The third is the one that matters, and it was produced by pointing
`DBUS_SESSION_BUS_ADDRESS` at a path that does not exist. **A missing gamemoded
must never fail a launch** — most machines have none — and the run that produced
that line went on to build its symbol table and load the engine exactly as the
other two did.

**MangoHUD**, in `launch.rs` and the Settings window. `MANGOHUD=1` on the client
process is the whole mechanism, because MangoHUD is a Vulkan implicit layer. The
work is not the switch, it is `launch::mangohud_layer`: **MangoHUD is not
installed on this machine**, and `MANGOHUD=1` with no layer present is not an
error — the client starts, no overlay appears, and nothing says why. So the
layer is looked for across the Vulkan loader's own implicit-layer search path
plus the Flatpak extension mount point, and when it is absent the Settings row
is insensitive and names the packages to install instead of being a switch that
appears to work. That defect has shipped on this page twice; this is the check
that stops a third.

## 1d(iv). What was considered from Lutris's list and rejected

The request behind §1d(ii) and §1d(iii) was to look at what Lutris does. Most of
it does not apply here, and recording that is half the value of this file:

- **DXVK, VKD3D, esync, fsync, every `WINE*` variable** — there is no Wine and no
  Direct3D anywhere in this project. Not applicable, not a judgement call.
- **`DRI_PRIME`, `__NV_PRIME_RENDER_OFFLOAD`, ICD selection** — hybrid-graphics
  selection is a real feature for a two-GPU laptop. This machine has one GPU, an
  Intel UHD (Raptor Lake-P), so **it cannot be tested here and must not ship
  untested**. Worth a later task on hardware that has two.
- **`mesa_glthread`** — GL only. The engine renders through Vulkan on this path
  (the GLES counters read zero in every run above while `vkQueuePresentKHR` read
  thousands), so it would do nothing. Rejected.
- **Shader cache location and size** — genuinely plausible and *not done here*.
  The engine already writes `shadercachevk.bin` into the profile, so the cache
  exists and is per-profile; what is unmeasured is whether it is being evicted or
  is too small, and a cache setting added without that measurement would be a
  knob nobody could tell was working. Left as a real candidate with a real first
  step: instrument whether the file is being rewritten between launches.

## 1e. Fullscreen moves the canvas through two wrong (position, size) pairings

Driven in-process with `gtk_window_fullscreen` from a scripted timer — allowed,
and not input injection — twice per direction in one 240-second run. What Cordial
actually sends, logged at the call:

```text
script: fullscreen
  set_position(0, 46) size=1280x721      <- fullscreen position, windowed size
  surface_resized -> 3440x1394
script: windowed
  set_position(12, 58) size=3440x1394    <- an intermediate inset, fullscreen size
  surface_resized -> 1280x721
  set_position(25, 71) size=1280x721     <- settled, about three seconds later
```

`sync_canvas_geometry` reads one `content_rect()` and applies its position and
its size together, so these are not Cordial tearing them apart — GTK reports the
rectangle in that order as the transition settles, and each intermediate is
faithfully forwarded. Exactly **one** `surface_resized` per transition, so there
is no swapchain-rebuild storm; the cost is that the canvas is visibly out of
register with the window for the seconds in between.

**Which size the intermediate `set_position(12, 58)` carries is a race.** Across
four leave-fullscreen transitions in two runs it was `3440x1394` twice and
`1280x721` twice — GTK had applied the position half of the transition and the
size half in either order by the time the pump sampled it. That is the same
non-atomicity §1b is about, arriving through the allocation rather than through
the buffer, and §1b's `set_sync`-while-resizing remedy is still the untried
candidate for both.

**Why it looks permanent.** At 1.0 present per second (§1d) the wrong frame stays
on screen until the engine draws again, and the engine draws again when the user
moves the mouse — which is exactly what dragging the window edge does. With input
flowing the same transition corrects itself in about three seconds and the rate
never leaves 50/s.

**Not reproduced: a state that stays broken.** The report is of a canvas that
stays wrong until the window is dragged. What is reproducible from a timer is a
transient. `gtk_window_fullscreen` may not exercise the same path as a
compositor-driven fullscreen, and the owner's case may involve a different size
or monitor. Anyone with a keyboard should check it by hand before this is called
closed.

### 1e (cont.) The swapchain *is* rebuilt at the fullscreen size. Measured 2026-08-03

The standing lead was that the extent never changes: on Wayland the swapchain
size does not come from the compositor at all —
`vk_get_physical_device_surface_capabilities_khr` substitutes
`wayland::current().geometry()` when Mesa reports the documented `0xFFFFFFFF`
"you choose" — and that geometry is written only by `apply_resize`, which
early-returns when the size is unchanged. If `apply_resize` never fired with the
fullscreen size, every swapchain after the transition would be the old size,
which is exactly the reported symptom.

**It fires, every time.** Eight transitions across two 150-second runs, build
`0.3.0-21-gf70ee23-dirty`, `--profile agent-text`, no `WAYLAND_DEBUG`:

```text
[instr] t= 40.0s script: fullscreen
[instr] set_position(0, 46) size=1920x1154
[instr] surface_resized -> 1920x1154
[android] vulkan: reporting surface extent 1920x1154 to the engine
[android] vulkan: vkCreateSwapchainKHR extent 1920x1154 (old swapchain recreated)
```

and the mirror image on the way back, `1280x721` in all four places. The
per-second geometry line moves in the same tick — `rect=Some((0, 46, 1920,
1154)) placed=(0, 46)` — so the content rectangle, the position, the engine's
`surface_resized`, the extent Cordial reports to Vulkan and the extent the
swapchain is built at are five numbers that all agree, in both directions,
twice per run, in two runs.

**It is not the idle throttle either.** One of the two runs fullscreens with
input off: presents are at exactly 1.0/s going in, the transition itself draws a
burst (4.9/s in that second), and the sequence above is identical. The engine
does redraw at the new size without a mouse; it simply redraws once a second.

**Nothing swallows `VK_ERROR_OUT_OF_DATE_KHR` or `VK_SUBOPTIMAL_KHR`, and the
driver never sent one.** `vk_queue_present_khr` forwards the return value
untouched and now also reports any non-success code, at the first of each and
then at each power of ten. Across both runs and every transition: not one line.
So "the engine ignored a suboptimal swapchain" and "Cordial ate the error" are
both off the table — there was no error to ignore or eat.

**So the three named suspects are all disproved and the bug is not reproducible
from `gtk_window_fullscreen`.** What is left is the difference between that call
and however the reporter fullscreens — and the one candidate with a mechanism is
the 0x0 configure: `content_rect()` returns `None` for a zero allocation and
`sync_canvas_geometry` used to return silently on it, which would leave the
canvas at its old size in a new-sized slot and correct itself the moment a
workspace switch forced a fresh allocation. That case now prints
`wayland: no content rectangle to place the canvas by`, once per run of them,
and the size it comes back with. **If the symptom is real, that line is the
first thing to look for in the log**, and if it is absent the geometry path is
not where the bug is.

**Reproducing all of the above:**

```bash
CORDIAL_INSTR=1 CORDIAL_SCRIPT=20:motion-on,35:motion-off,40:fullscreen,\
60:motion-on,75:motion-off,80:windowed,100:fullscreen,130:windowed \
  just client --run 150
```

**New, and a real bug: the X11 backend segfaults on a fullscreen transition.**
`backend_set_fullscreen` was Wayland-only, so nobody had ever driven one there;
it now sends `_NET_WM_STATE_FULLSCREEN` on X11 too, purely so that a fullscreen
transition can be *photographed* (see below). Two runs out of two, the client
dies within a second of the transition, immediately after
`vkCreateSwapchainKHR extent 1920x1163 (old swapchain recreated)`. The kernel
names an engine thread and the same faulting offset both times:

```text
Main[538171]: segfault at 44 ip 0x7fbb4e9af4c1 ... +0x13a4c1 (a ~10 MB mapping, not libroblox)
Main[546315]: segfault at 44 ip 0x7fc8cc90c4c1 ... +0x13a4c1
```

Deterministic, and inside a library about the size of a Mesa driver rather than
inside the 115 MB engine. **`INFERRED`:** that this is the resize rather than
fullscreen as such — nothing has ever resized an X11 Cordial window from
automation either, so "X11 cannot survive a swapchain rebuild at a new size" and
"X11 cannot survive fullscreen" are not yet distinguishable. Wayland does the
same transition eight times without complaint.

**Why X11 was dragged into this at all: you cannot photograph a Wayland surface
on this desktop.** `org.gnome.Shell.Screenshot.Screenshot` and
`ScreenshotWindow` both answer `AccessDenied`, `grim` is wlroots-only, and
`import` cannot see a native Wayland surface — so on Wayland every claim in this
section is a claim about numbers, not about pixels, and it is stated that way
deliberately. `import -window` does work against the X11 backend, which is why
this session's text-entry screenshots (§1) are all X11.

**Tried and it did not help: `onSurfaceRedrawNeededNative` on a geometry
change.** `window.rs` drives that native from the final X11 `Expose` and this
backend drove it from nowhere at all, so the argument was that an idle engine has
nothing telling it the canvas moved. Two otherwise identical 240-second runs,
minutes apart in one session, over the idle fullscreen cycle: **~75 presents
without the call and ~74 with it**, and the per-second shape is the same either
way. The engine already repaints on `surface_resized` by itself. The call was
removed again rather than left in looking like a fix; `sync_canvas_geometry`
carries a comment saying so, so that the next person reaches for something else.

**How both of the above were driven, since it needs no human.** `CORDIAL_INSTR=1`
plus `CORDIAL_SCRIPT=20:motion-on,70:fullscreen,100:windowed,130:motion-off,
160:fullscreen,190:windowed,220:motion-on` runs the whole timeline from
`looper::pump` in one launch — `gtk_window_fullscreen` for the transitions and
Cordial's own `input::deliver_touch`/`pass_mouse_move` for the pointer, so no
compositor is involved and nothing can reach the developer's session. One launch
covers both fullscreen directions twice, with and without input, and prints
presents, looper polls, pump iterations and the content rectangle every second.
Use it instead of a handful of short runs; every launch is a window on somebody's
desktop.

**Three readings from a `WAYLAND_DEBUG=1` run that are wrong, retracted here
before they mislead anyone.** That run showed (a) the pump thread emitting no
tick for 12.6 s after leaving fullscreen, (b) presents at 0-3/s for a further
twelve seconds with input still flowing, and (c) fullscreen running at 20-25/s.
All three looked like findings. All three are artefacts of the tracer: the same
script without `WAYLAND_DEBUG`, minutes later in the same session, has **no tick
gap over two seconds anywhere in the run**, holds 50/s straight through both
transitions, and averages 49.4/s in fullscreen. `WAYLAND_DEBUG` writes a line per
request on a connection three parties share. **Do not measure timing under it** —
use it for object identity and request order, which is what it is good for and
what settled §1c.

## 2. Sign-in itself

Without a session the client sits on the landing page. Avatar thumbnails fail
against user id 0 and there is nothing to do. `NativeUserJavaInterface` is
stubbed with an empty user.

**[`docs/design/sign-in.md`](design/sign-in.md) is the investigation.** Read it
before starting; it is careful about what is verified and what is inferred.

The short version: the blocker is **obtaining a session cookie**, not the stub
code. The engine's own HTTP client takes 401/403 from authenticated endpoints
regardless of what the Java-side user stubs return, so filling those in changes
nothing on its own.

**Good news, and it changes the plan: plain login does not need a WebView.**
Lua-rendered login is the *shipped default* in this build, established three
ways — the dex bytecode for what the native gates, the flag that controls it, and
the shipped content itself, which carries a full `Authentication.Login.*` string
table and a `LoginNative` screen name while `LoginWeb`/`LoginWebView` return zero
matches.

Reproduced here with a control:

```text
default                          nativeIsLuaLoginEnabled() -> true
FIntLuaAppLoginMethod=0          nativeIsLuaLoginEnabled() -> false
```

`CORDIAL_SIGNIN_PROBE=1` asks the engine directly.

**Captcha is still narrowed rather than settled** — there is a `CaptchaNative`
screen name, but also `Turnstile` and `CaptchaV2` strings suggesting
server-selected backends, so budget for a WebView on that path even though the
login form itself does not need one.

One correction that came out of it: the `The requested Ids are invalid`
thumbnail failure is what the **real, logged-out Android client** also produces.
It is not a Cordial defect and it is not evidence of anything.

## 2a. `CORDIAL_COUNT_GL=1`: not broken, just answering "is GLES running?" — and the
answer to that is always no

**Correction to what this section used to say.** It reported zero for
`eglCreateWindowSurface`, `eglSwapBuffers` and `glClear` on *both* backends and
concluded the instrument was broken. It is not. `vkQueuePresentKHR` on the
*same* counter reads real numbers on both backends now (668 on X11, 663 on
Wayland, for comparable runs) — checked directly with `CORDIAL_ANDROID_TRACE=1
CORDIAL_COUNT_GL=1`. The EGL counters read zero because **the engine renders
through Vulkan on both backends in this build, not GLES**, confirmed the same
way: `CORDIAL_ANDROID_TRACE=1` alone shows `vkCreateInstance`/
`vkCreateAndroidSurfaceKHR -> vkCreate{Xlib,Wayland}SurfaceKHR` on every launch,
X11 or Wayland, with no `eglCreateWindowSurface` ever appearing in the same
trace. A GLES counter reading zero is not a broken instrument here, it is a
correct answer to a question ("is the GLES path active") whose answer is no on
this hardware/driver combination. The earlier entry conflated "this counter
reads zero" with "this counter is unreliable" without checking which renderer
was actually live — exactly the control this file's own "Measuring anything"
section asks for, skipped.

The specific "suspected, not confirmed" theory this section used to carry —
that `android::mod::overrides()` appending `glcount::overrides()` *after* the
backend's own list lets the counting wrapper silently replace `window.rs`/
`wayland.rs`'s `eglCreateWindowSurface` — is **confirmed false**, by reading
`glcount::overrides()` directly rather than running anything: it registers
`eglMakeCurrent`, `glClear`, `glDrawElements`, `glDrawArrays`,
`glCompileShader`, `glTexImage2D` and `eglSwapBuffers` (or `swap_buffers_timed`
under `CORDIAL_SWAP_TIMES=1`) — no `eglCreateWindowSurface` entry at all, so
there is no key for it to collide with in the `BTreeMap` `symtab::build`
collects overrides into. It cannot be replacing something it never names.

**Still true and still the right instinct:** do not use `CORDIAL_COUNT_GL=1`'s
*EGL* counters to decide whether a backend renders, because on a Vulkan build
they will read zero regardless. `vkQueuePresentKHR` under the same flag is
reliable and is what actually answered "does Wayland ever present a frame" —
see §1a above. The engine's own FLog is close to useless for this specific
question: `SurfaceController`/`RenderJob` log at startup and then go silent for
the rest of the session on both backends, identically, so there is no
per-frame signal to read there either. What works is the Vulkan call counter,
or looking at the window.

## 2b. Audio never initialises before sign-in, and AAudio is not why

The OpenSL ES backend over PipeWire works in a standalone harness and has never
been seen carrying a single sample through the real client. The reason is not a
bug in it.

> **Correction (audio device work).** The paragraph above was right about
> laziness and wrong to stop there, because it was written without checking
> whether the backend was in the binary at all. It was not. `pkg-config` first
> on `PATH` on the development machine is Homebrew's `pkgconf`, whose
> compiled-in `pc_path` is its own Cellar directories and excludes
> `/usr/lib64/pkgconfig`, so `pkg_check_modules(PIPEWIRE ...)` reported
> libpipewire-0.3 missing on a host with pipewire-devel 1.6.8 installed.
> `CMakeCache.txt` recorded `PIPEWIRE_FOUND:INTERNAL=` and
> `cordial_liblog.dir/flags.make` recorded an empty `CXX_DEFINES`: every
> release build had compiled the `#else` branch of `pipewire_backend.cpp`, and
> `slCreateEngine` had been reporting `SL_RESULT_FEATURE_UNSUPPORTED` for a
> reason that had nothing to do with audio. `cordial_pipewire_backend_test` had
> never run either, for the same reason. `native/CMakeLists.txt` now falls back
> to `find_path` for the headers — the only thing that is wanted, since the
> library is dlopen'd — and the build reports
> `-DCORDIAL_HAVE_PIPEWIRE=1`.
>
> **The conclusion below still holds after that fix, and was re-measured.**
> Three 30-second runs to the Landing screen with the backend genuinely
> compiled in produced no `slCreateEngine` call at all (the backend prints
> `PipeWire session confirmed reachable` on its first use; it never appeared),
> so audio initialisation really is lazy and really does need something past
> sign-in. What changed is that this is now a statement about Roblox rather
> than, unknowingly, a statement about the build.

**Roblox makes exactly one `dlopen` in a 75-second run to the Landing screen:**

```text
[cordial] dlopen(libroblox.so) -> ok in 21896us
```

That is Cordial's own load. `CORDIAL_TRACE_DLOPEN=1` reports every request and
how long it took. Nothing else is asked for — no audio backend, and no
`libvulkan.so` either, which is the control: with no `flags.json` the engine
picks GLES, so the absence of a Vulkan request confirms the trace catches real
calls rather than missing them.

**The AAudio-preference theory does not survive the linkage.** `strings` shows
`FmodFallbackAaudioToOpensl`, and FMOD does prefer AAudio. But:

| | |
|---|---|
| `libOpenSLES.so` | in `DT_NEEDED` — *linked*, so `slCreateEngine` is directly callable and needs no `dlopen` at all |
| `libaaudio.so` | not in `DT_NEEDED`, and **zero** `AAudio*` undefined symbols |

So AAudio is reachable only through `dlopen`, and that `dlopen` never happens.
FMOD's backend selection has not run. Cordial providing a `libaaudio.so`, or not
providing one, cannot currently make any difference — there is nothing to fall
back *from*.

**Therefore audio initialisation is lazy, not eager**, and reaching the
logged-out Landing screen is not enough to observe it. Verifying the PipeWire
path through the real client needs something that actually plays a sound, which
means getting past sign-in. It is blocked on §1, not on itself.

**Do not re-run:** adding a virtual `libaaudio.so` to make FMOD fall back. There
is no evidence FMOD has initialised, and the fallback string is not evidence that
it has.

**Voice chat is a different path and is not covered by any of this.** The
real-Android capture has `MainScreenController: Initializing RTC audio manager`
during startup — that is WebRTC, separate from FMOD, and it is the only audio
line in the whole capture. FMOD does not log to logcat at all, which is why the
capture cannot answer the eager-versus-lazy question and the `dlopen` trace had
to. Note also that `SL_IID_RECORD` is among the referenced symbols and
`native/opensles.cpp` deliberately refuses recorder creation: correct for now,
and exactly what voice chat will need implemented later.

## 2c. Deep links reach the engine. Whether they join needs an account

`cordial-run --join-url <url>` takes a `roblox-player://` or `roblox://` link
from a browser click and hands it to the engine.
[`docs/analysis/deep-links.md`](analysis/deep-links.md) is the investigation and
`crates/cordial-runtime/src/deeplink.rs` is the code.

**The engine asks nobody for a URL.** No `Intent`, no `Uri`, no `getIntent` —
checked in a full launch and in the Waydroid capture, where every `Intent` line
belongs to Google Play services rather than to Roblox's process. The URL is
delivered *to* the engine, which makes this Cordial's statement to make rather
than a question to answer.

**What works, measured twice with a control.** Publishing on the engine's own
linking message during bring-up:

```text
MessageBus.publishRaw("Linking.detectURL", "{\"url\":\"roblox://…\"}")
```

makes the app shell answer, by the first `APP_READY`, with

```text
Game.launch  {"placeId":1818,"referralPage":"DeepLink","joinAttemptId":"fe7bec78-…"}
```

`placeId` and `referralPage` are the engine's words — Cordial passes the URL
through as one opaque string and never parses it. `isColdStartDeeplinkToGame()`
goes false -> true across the same delivery. `CORDIAL_DEEPLINK_NO_PUBLISH=1` is
the control: identical launch, publish suppressed, neither observable moves.

**Two things are not done, and the first is the important one.**

*Whether it joins is unverified and cannot be verified without an account.*
`Game.launch` is the app shell asking for an experience; every run here ends at
`app ready: Landing`, because a signed-out client belongs there. Closing this
needs §2 first, and then one signed-in launch with `--join-url`.

*`roblox-player://` links are translated, and the translation reaches the engine.*
The engine's own pattern, the client setting `FStringGameLaunchLinkURL`, matches
`roblox://` and `robloxmobile://` and no other scheme — measured, not read off
the regex alone. That is the scheme roblox.com's desktop play button emits and
the handler Cordial took from Sober, so Cordial now takes the desktop format
(`roblox-player:1+launchmode:play+gameinfo:<ticket>+placelauncherurl:…`) apart,
carries the `placeId` out of the decoded launcher URL into
`roblox://experiences/start?placeId=<id>`, and publishes that. Measured twice
with `CORDIAL_DEEPLINK_NO_TRANSLATE=1` as the control twice: with the rewrite the
app shell answers `Game.launch` naming the place, without it nothing does
(`docs/analysis/deep-links.md` §6.1).

The one-time `gameinfo` ticket is dropped — this engine is the Android client and
has no such ticket in any link it accepts — and **whether a join survives that is
still unverified**, for the same reason nothing else about joining is: it needs a
signed-in account. A desktop link that names a *particular server*
(`accessCode`, `linkCode`, `reservedServerAccessCode`, `gameId`, `jobId`) is
refused rather than translated, because a link that joins the wrong server is
worse than one that does not join.

`CORDIAL_DEEPLINK_PROBE=1` prints the linking protocol's own message and field
names, read out of the running engine — that is how they were established, and
it is the cheap way to check whether a Roblox update renamed any of them.

## 3. Plugins: running, but with three methods

`crates/cordial-plugins` has capabilities, a broker, manifests, user grants and a
Deno host. `crates/cordial-runtime/src/plugin_host.rs` joins it to the client:
plugins are discovered, started after bring-up, and served from the real flag
resolver. Verified in a live launch — the example plugin in
[`plugins/flag-inspector`](../plugins/flag-inspector) reads a flag the user
actually set and is refused a capability it did not request.

**Update, still true where it matters:** `presence.set`/`presence.clear`,
`notify.send`, `url.open`, and the three `events.*` methods (ADR-006) are now
real, effect-performing brokers — Discord IPC framing, the freedesktop
notification and OpenURI portals, and an event registry with ownership rules
— all implemented and tested in `crates/cordial-plugins` (see `presence.rs`,
`notify.rs`, `urlopen.rs`, `events.rs`, and `host.rs`'s `Session`, which is the
single dispatcher that authorises and performs all of them). `lifecycle.read`
now has something to push, too: `Session::push_lifecycle` delivers `launch`,
`ready` and `shutdown` to any plugin holding the capability. A first-party
plugin, [`plugins/discord-presence`](../plugins/discord-presence), exercises
the whole path — verified against a local Discord IPC test double, not a real
Discord client (none is available where this was built); see the plugin's own
source comment.

**What is still missing is the join, not the brokers.** `serve()`'s `dispatch`
in `crates/cordial-runtime/src/plugin_host.rs` is a separate, older function
that predates `Session` and still only answers `flags.list`, `flags.get` and
`log.write`, falling through to `error: not implemented yet` for everything
else — including the four methods above, which now have real implementations
sitting unused one crate away. `plugin_host.rs` was out of scope for the work
that added them (other agents were active there), so the live client still
cannot broker Discord presence, notifications, URL-opening or plugin events
until `dispatch` is replaced with (or delegates to) `cordial_plugins::host::
Session::handle`, and `push_lifecycle` is called from wherever the client's
own launch/ready/shutdown transitions are detected. That wiring, plus
`flags.write` and `flags.write.dynamic`, is what remains.

See [ADR-003](adr/ADR-003-plugin-isolation.md) for why isolation is by process,
[ADR-004](adr/ADR-004-plugin-asset-overrides.md) for why plugins cannot replace
Roblox's assets, and [ADR-005](adr/ADR-005-flag-service.md) for why flag writes
are two capabilities.

---

## 4. Accessibility — SETTLED 2026-08-21: Roblox exposes no tree, so the bridge is unreachable

**The open question below is answered and the answer is negative.** Roblox's
Android client publishes no accessibility tree by any mechanism. `libroblox.so`
has 517 `Java_*` exports and none mention accessibility; no `com/roblox/**`
class in the dex is named `*Accessib*` or implements a provider; and a 40 s run
with the AT-SPI bridge genuinely attached — so the `isEnabled` gate answered
true honestly — produced zero calls into `native/accessibility.cpp`. The
inference that the engine "was built with TalkBack support compiled in" came
from the dex *referencing* those classes, and a referenced class is not a used
class. Full working in the header comment of `native/accessibility.cpp`.

Consequence worth carrying: any development or automation surface for Cordial
has to work in coordinates and pixels. There is no semantic element tree to
read, and getting one would mean engine introspection, which ADR-001/ADR-003
place permanently out of scope.

The original entry follows, retained for its reasoning.

New in this change: `native/accessibility.cpp` hooks
`android.view.accessibility.{AccessibilityManager,AccessibilityNodeInfo,
AccessibilityEvent}` the same way every other class in `android_classes.cpp`
answers a platform service, mirrors whatever the engine populates into a
small registry, and `crates/cordial-runtime/src/android/accessibility.rs`
republishes that registry as a real `org.a11y.atspi.*` application on the
accessibility bus — Linux's TalkBack equivalent, which is what Orca and other
screen readers actually read.

**This was written and tested with no Roblox APK available in the
environment.** `crates/cordial-runtime/src/bin/load.rs --apk` needs one the
user supplies, and none was reachable — a Waydroid instance was present but
came back from a freeze/thaw cycle with a broken guest network (`adb`: `no
route to host`, persisting past several retries and a `waydroid show-full-ui`
re-attach; not debugged further, since resurrecting one stale container is
not the point of this change). That has two consequences, and they should
not be conflated:

**Verified live, with evidence, no Roblox involved:** the AT-SPI-facing half
of the bridge. `crates/cordial-runtime/examples/accessibility_probe.rs`
seeds three synthetic nodes (a button, a checkbox, a label — clearly labelled
as fixtures, never Roblox data) straight into the same C++ registry
`AccessibilityNodeInfo`'s real hooks write to, then starts the bridge for
real. Queried externally over the actual accessibility bus:

```text
$ busctl --address="unix:path=/run/user/1001/at-spi/bus" tree :1.253
└─ /org/a11y/atspi/accessible
   ├─ /org/a11y/atspi/accessible/node
   │  ├─ /org/a11y/atspi/accessible/node/1
   │  ├─ /org/a11y/atspi/accessible/node/2
   │  └─ /org/a11y/atspi/accessible/node/3
   └─ /org/a11y/atspi/accessible/root
```

with `GetRole`/`GetRoleName`/`GetState`/`GetExtents` all reading back
correctly (`push button` for the seeded button, `[1124075776, 0]` for its
state word — hand-verified against `ATSPI_ROLE_PUSH_BUTTON`/the
`AtspiStateType` ordinals in `/usr/include/at-spi-2.0/atspi/atspi-constants.h`,
bit-for-bit), and — after fixing a real bug found this way, not guessed —
`org.a11y.atspi.Registry`'s own tree lists Cordial as an embedded
application, meaning `busctl --user tree org.a11y.atspi.Registry`-style
discovery works, not only a direct connection to a known bus name.

**The bug, for the record:** the first `Socket.Embed` call sent
`&(bus_name, ROOT_PATH)` as the method body, which `zbus`/`zvariant` encodes
as *two* top-level arguments (`ss`) rather than the *one* struct-typed
argument (`(so)`) the real method takes — confirmed via `gdbus introspect`
against the live registry before writing the fix. The registry's own daemon
did not error, it simply never replied (`NoReply: Remote peer disconnected`),
which would have been very easy to misdiagnose as a permissions or
bus-address problem rather than a wire-format one. Fixed in
`android::accessibility::connect` by wrapping the struct in an extra
one-element tuple and using a real `OwnedObjectPath` rather than a bare
`&str` for the path half. **Correction to this task's own brief:**
`busctl --user tree org.a11y.atspi.Registry` alone does not reach the
accessibility bus — `--user` targets the *session* bus, and the AT-SPI bus is
a separate socket obtained via `org.a11y.Bus.GetAddress`; the working form is
`busctl --address="unix:path=<that address>" tree org.a11y.atspi.Registry`.

**Not verified, and not claimed:** anything about what Roblox's engine
actually does. `native/accessibility.cpp`'s own header comment lays out the
structural question this leaves open — real Android's accessibility tree is
*pull* (`AccessibilityNodeProvider`, Java/Kotlin app code the platform calls
into on demand), not *push*, and per this project's own established finding
on `MainGameActivity.bootstrapTheApp()`, Java/Kotlin application logic
cannot execute under Cordial at all. If Roblox's Android build implements
accessibility that way, no amount of hooking `AccessibilityNodeInfo` reaches
it, for the same structural reason hooking getters alone never reached
FastFlags bootstrap. What *is* plausible, and is what this file is written
to catch if true, is the engine building nodes directly over JNI the way it
does everything else in `android_classes.cpp` (a native-to-Java push, no app
subclass involved) — but only a live run with `CORDIAL_ACCESSIBILITY=1
CORDIAL_JNI_TRACE=1` (or `--dump-classes`) against a real APK, past sign-in,
with a genuine assistive technology attached, distinguishes the two. **Do
this before claiming Cordial makes Roblox screen-reader-usable** — everything
in this section is "the pipe works", not "there is water in it".

**Also not done, on purpose:** forwarding
`AccessibilityManager.sendAccessibilityEvent` as a real AT-SPI signal — it is
captured (see `cordial_accessibility_next_event`) and currently only logged
to stderr by the poll loop, because getting `org.a11y.atspi.Event.Object`'s
own signal shapes right needs the same live-verification treatment `Embed`
just got, and this change was long enough already. `Action::DoAction` also
always answers `false`, honestly rather than as a stub that lies — see
`android/accessibility.rs`'s own header comment on why there is no receiver
for an invoked action to reach yet, the same shape of gap as the provider
question above.

**Deaf users need nothing from this work** — captions and visual alerts are a
different, unbuilt piece of work; nothing here touches it, and nothing cheap
and real for it turned up while doing this one.

---

# Do not re-run these

Each was tested and each cost time. The evidence is the point.

**The futex that used to hang startup**
- Never an EGL/GBM surface handshake. It is the engine's ordinary wait primitive
  — offset +0x0C of a 64-byte-aligned object, the same class and call site all
  sixteen idle `RBX Worker` threads park on.
- Never an unserviced `ALooper`. Tested directly: the main thread pumped
  `epoll_wait` continuously on a dedicated thread while the worker still blocked
  at the identical futex.
- It was in `nativeAppBridgeStartLuaAppDM`, not `StartAppWithParams`.
- The block and the crash were **one bug, not two** — a completion handshake with
  a thread that segfaulted before it could signal.

**The frame rate**
- Window focus is not it. `onWindowFocusChangedNative(true)` and
  `onContentRectChangedNative` are both sent; the rate did not move.
- Frame-callback starvation is not it. `AChoreographer_*` is not imported at
  all, and — checked separately on 2026-08-20, because the NDK entry points and
  `android.view.Choreographer` reached through JNI are different paths and only
  the first had been ruled out — the engine does not ask for the Java one
  either. `--dump-classes` over a 30 s run lists 182 classes and no
  Choreographer. (`docs/analysis/framework-classes.txt` does list it; that file
  is a framework inventory, not a request log.)
- `FIntReactSchedulerMinFrameRate` set to 60 changed nothing.
- The render job binds its DataModel fine — `No DM yet` appears exactly twice,
  transiently, around 2.0 s.
- Graphics-quality FastFlags do nothing here. `DebugFRMQualityLevelOverride` and
  both MSAA overrides, at every prefix, left shader count, target size and frame
  rate identical. They govern 3D scene rendering and the landing page is a 2D
  interface. The hardware reports MSAA 16 support, so this is not a capability
  limit.

**The crash that used to kill a third of launches**
- Not a `pthread_create` override skipping per-thread setup — there is no such
  override, it is a plain passthrough.
- Not a `pthread_mutex_t`/`pthread_attr_t` ABI mismatch.
- Not a `malloc`/`free`/`operator new` mismatch directly — none of those are
  undefined symbols in `libroblox.so` at all.

**`onKeyDownNative` is registered, and the code it receives is an Android
keycode**

Worth stating because the opposite was a live theory: only the D key appears to
work in an experience, and evdev `KEY_D` and `AKEYCODE_D` are both 32 — they
collide at exactly one letter, so a raw evdev code reaching something that
wanted an Android one would look precisely like that. It is not happening at
this layer. Measured, with `CORDIAL_ANDROID_TRACE=1` on two consecutive runs:

```text
[android] onKeyDownNative(code=31) -> true      <- C, evdev KEY_C is 46
[android] onKeyDownNative(code=40) -> true      <- I, evdev KEY_I is 23
[android] onKeyDownNative(code=37) -> true      <- H, evdev KEY_H is 35
[android] nativePassKeyEvent(down=true, keyCode=51, modifiers=0x0) -> Ok(())
```

So the AGDK native is in the natives table, it returns `true`, the codes are
`AKEYCODE_*`, and `NativeGLInterface.nativePassKeyEvent` resolves and returns
cleanly on the same keystroke. Whatever makes only D work in an experience is
downstream of both, or is not about keycodes at all. `nativePassKeyEvent` is now
traced under `CORDIAL_ANDROID_TRACE=1`, which it never was before — every
keyboard investigation until now read only the AGDK half.

The reason none of this was visible before: `deliver_key`/`deliver_touch`
answered "the native is not registered" with silence, so a trace run that
printed nothing was indistinguishable from a trace run whose events were all
dropped. They now say so by name, at the first drop and then at each power of
ten — once would be indistinguishable from the normal startup race against
`initializeNativeCode`, and per event would bury the log.

**Resizing the window reflows the interface into many small items — and density
is not the fix**

Widening the window makes Roblox lay out more, smaller things: at roughly
1330px the home feed shows four recommended tiles at a comfortable size, at
2000px it shows six much smaller ones. The cause is understood. `DisplayMetrics`
in `native/init_params.cpp` reports `density = 1.0`, `densityDpi = 160`, and
Android's density is the scale against 160 dpi — so a 2000-pixel window is
described to the engine as a 2000dp-wide screen, which is a tablet the size of a
wall, and the client lays out for one. That is correct Android behaviour and the
wrong thing for a desktop monitor.

Correcting the density was tried and is **reverted**. Raising it (to the
compositor's output scale times 1.5, feeding both `DisplayMetrics` and
`PlatformParams.dpiScale` from one number) was measured by the owner in normal
use as having "absolutely destroyed the DPI on roblox and it still didnt fix the
resizing issue" — worse in the everyday case and no better in the case it was
for. `CORDIAL_DPI_SCALE` remains what it was, an override that changes
`PlatformParams.dpiScale` only.

**The engine reads its density exactly once, and a resize does not make it read
again.** Measured, not inferred: with a counter on every `DisplayMetrics`
construction, one 46-second run printed

```text
[android] DisplayMetrics #1: 1280x720 density=1.000 densityDpi=160
```

from inside `initializeNativeCode` — before the host window exists — and printed
nothing further, including across a live resize from 1280x721 to 2000x1100
driven by `gtk_window_set_default_size` from a timer. `onSurfaceChangedNative`
and `onContentRectChangedNative` are both re-driven on that resize, so the
engine does learn the new size; it simply never re-reads the density. Anything
that hopes to change density in response to a resize therefore cannot work
through this object, and a version that appears to work is a version that is
doing nothing. **`INFERRED`:** Android would deliver such a change as a
configuration change, which nothing here drives; whether the engine would honour
one is untested.

Consequence for the ordering, if anyone does revisit this: the density has to be
settled *before* `initializeNativeCode`, which is earlier than the host window
exists and therefore earlier than the display can be asked about itself.

Resizing is not currently a goal. Do not leave a partial or
disabled-by-default mechanism for it in the tree.

**Flags**
- The flags verdict does not gate rendering. `onFlagsFailed` is a complaint, not
  a gate.
- `nativePreloadFlagOverrides` does nothing observable, despite the name. Merging
  into the client-settings document is the mechanism that works.
- **Do not test whether flags work using FLog channels.** Setting
  `FLogAndroidGLView=7` produces no output even when flags demonstrably work, so
  it is a broken instrument — it produced a confident, wrong "no flag reaches the
  engine" that survived several experiments. Use a flag with an observable
  behavioural effect, and run a control.

**`--lib-dir` without `--host-libc`, and the five pthread symbols**

`pthread_once`, `pthread_key_create`, `pthread_key_delete`,
`pthread_getspecific` and `pthread_setspecific` are implemented, so nothing
needs `--host-libc` for them. They are thin forwards to the host's libc, which
is safe *for these* because the types involved are laid out identically:
`pthread_once_t` and `pthread_key_t` are both 4 bytes in both libcs and both
spell `PTHREAD_ONCE_INIT` as 0, measured off this tree's bionic headers rather
than assumed. Compiling bionic's own `pthread_key.cpp` instead would have been
worse than the stub it replaced, because it reads thread-specific data out of
`__get_tls()[TLS_SLOT_BIONIC_TLS]` — bionic's thread structure, and every thread
in this process belongs to the host's libc, so it would have appeared to work.

**That does not make bare `--lib-dir` a working configuration, and nothing short
of a real libc shim will.** It stubs 358 libc symbols, `memset`,
`pthread_mutex_lock` and `newlocale` among them. Measured after the change: the
load runs further into the engine's static initialisers and ends in SIGSEGV with
`__cxa_atexit` the last stub reported, where before it exited 1 on the
fatal-stub guard at `pthread_once`. Which stub it now cannot survive is *not*
established — `memset` is the obvious guess and the same run disproves it, since
it called `memset` and carried on through five more first-hit stubs. The fatal
list in `stubs.rs` is empty as a result. Put a symbol in it when a run shows the
process dying on that symbol, not because it looks dangerous.

`--host-libc` and `--game-activity` were checked in the same session and are
unaffected: exit 0 with `=== no stubs were called ===`, and `app ready: Landing`
with two ZSTD trace stubs in a 10-second run.

**Corrected on the way: `pthread_cond_t` is the same size in both libcs.** The
commit that introduced `bionic::pthread` recorded "pthread_cond_t is 32 bytes in
bionic, 48 in glibc" as one of three ABI divergences found, and the module's
doc-comment table repeated it. It is wrong. 32 bytes is `pthread_barrier_t`,
`int64_t __private[4]`; `pthread_cond_t` is `int32_t __private[12]` and comes to
48 on LP64, the same as glibc's. Measured by compiling one probe translation
unit twice — `char sz_x[sizeof(x)];` per type, sizes read back with `nm -S` —
once against `third_party/mcpelauncher-linker/bionic/libc/include` at
`-target x86_64-linux-android`, once against the host's glibc. `sem_t` at 16
against 32 is a real mismatch and its wrapper is load-bearing; the condition
variable wrapper is not, and was written for an overrun that could not happen.
It is left in place because removing it changes what runs at every
`pthread_cond_wait` in the engine, which wants its own measurement.

## 5. System time equals user time — and it is almost all the engine's

> **2026-08-21:** still true, and §0b now says what the engine is doing with it —
> one thread on `ALooper_pollOnce(0, ...)`, which Sober also has. The attribution
> below stands; the conclusion once drawn from it, that Cordial is unusually
> expensive, does not.


A 30s run spends about as long in the kernel as in userland (17.3s user, 16.8s
system) and racks up roughly 49,000 voluntary context switches. That looked like
Cordial's pump loop thrashing. It is not.

Measured across four full 30s runs against the real APK on Wayland, sampling
every thread's `wchan` and its own `voluntary_ctxt_switches` counter from
`/proc/<pid>/task/*/status` every 200ms:

| | share of voluntary switches | parked in |
|---|---|---|
| `HttpClient` (engine's network thread) | ~57% | `poll_schedule_timeout` — a scheduled timer, not socket I/O, cycling ~900/s while otherwise idle |
| `RBX Worker A`–`P` (engine task pool, one per core) | ~36% | `futex_do_wait` |
| Cordial's own thread running `pump()` | ~4% | `ep_poll` — the single deliberate 50 ms-bounded `epoll_wait` |
| other engine threads named `Main` | ~3% | |

So **~93% is inside `libroblox.so`**, which is not Cordial's to fix under
[ADR-001](adr/ADR-001-in-process-hooking.md). The per-iteration non-blocking
`poll(fds, 1, 0)` on the Wayland fd and `wl_display_flush()` contribute no
blocking `wchan` at all — they show up in the thread's running samples, not its
sleeping ones. Cordial names no thread `Main`; those four are the engine's.

**Disproved: `FIntTaskSchedulerAutoThreadLimit` is not a lever here.** Set to 1
and to 2 (against an unset default matching core count), verified reaching the
engine by the `flags: 1 override(s) applied` line. `RBX Worker A`…`P` still all
spawn, and user time, system time and switch count are indistinguishable from
baseline across every run. Negative result, repeated, recorded so nobody spends
an afternoon on it again.

**The 1,274 major page faults in the original measurement were environmental.**
Same harness with ~3 GB less swap pressure: single and double digits. Do not
read fault counts taken on a thrashing machine as a property of the client.

---

# How to work on this

## Debugging facts that cost real time

- **Reach for `tools/cordial-mcp.py` first.** `just dev --play` starts a client
  and `just mcp` attaches; it screenshots out of the swapchain (unaffected by
  occlusion, and the only thing on this host that can photograph a Wayland
  window -- five other routes were tried and every one was refused), drives
  Cordial's own input, and attaches lldb with the process CPU quoted beside the
  stacks. `cordial_info` twice is the wedged-or-throttled test. Written up in
  [ADR-019](adr/ADR-019-development-control-surface.md).
- **lldb is installed through Homebrew, not `dnf` or a container.** This host is
  immutable ostree, so `dnf` needs `rpm-ostree` and a reboot; a containerised
  debugger installs cleanly and then cannot attach at all, because rootless
  podman puts the tracer in a user namespace that is not an ancestor of the
  tracee's and no combination of `--privileged`, `--pid=host` and `SYS_PTRACE`
  repairs that. `yama/ptrace_scope` is already 0 here, so that is not it either.
  `brew install llvm` needs neither root nor a reboot.
- **lldb breakpoints inside `libroblox.so` do not work and fail silently.**
  Cordial `mmap`s it outside the system linker, so lldb never lists the image and
  every breakpoint stays unresolved with hit count 0. Use `memory write` of
  `0xCC`, then rewind `$pc` and restore the byte on trap. Crash-stop backtraces
  and breakpoints in Cordial's own code work normally.
- **Read syscall arguments from `/proc/<pid>/task/<tid>/syscall`** while lldb has
  the process stopped, not from registers. Number plus all six arguments, no
  guesswork about the libc wrapper's register shuffling. That is how the futex
  was identified without disassembling anything.
- **There are three threads named `Main`.** Use `thread backtrace all`.
- **`CORDIAL_TRACE_PATHS=1` is safe** and logs every path-taking libc call with a
  thread id. **`CORDIAL_TRACE=1` is not** — it wraps variadic functions with
  fixed-arity declarations and aborts the engine.
- **`CORDIAL_SKIP_AGDK=1` skips the flag and app-bridge calls entirely.** Several
  historical results were measured on a path that never ran the code under test.
- With ASLR disabled `libroblox` loads at `0x7fffefec0000`.
- lldb is at `/home/linuxbrew/.linuxbrew/bin/lldb`. No gdb, no strace.
- **Never inject input with `XTestFake*`** — it takes over the real cursor on the
  machine you are using.
- **The `XSendEvent` advice that used to sit here is dead.** It said to target the
  window by `WM_CLASS` `cordial`. Since [ADR-011](adr/ADR-011-wayland-and-libadwaita.md)
  there is no X11 window to target, and an agent following it lost most of a
  session discovering that — Sober is native Wayland too, and forcing
  `XDG_SESSION_TYPE=x11` only makes SDL fail outright because the X11 socket is
  not forwarded into the flatpak on a Wayland session. **Wayland has no
  window-targeted input injection at all**, by design: `wlr-virtual-keyboard` and
  the `RemoteDesktop` portal both inject at the compositor and land on whatever
  has focus, which is the category the rule above forbids.
  - **For Cordial's own window, do not synthesise at the protocol level.** Cordial
    *is* the Wayland client, so call the path directly — `input::pass_key_event`
    and `input::pass_text` are `pub`, and `wayland.rs`'s `dispatch_key` exercises
    the keysym translation above them. No compositor is involved and nothing can
    reach the developer's session.
    - **There is now a seam for exactly this and it needs no code:**
      `CORDIAL_INSTR=1 CORDIAL_SCRIPT=32:click:640x308,34:type:abc` clicks and
      types on a timeline, through `input::script_click`/`script_type`, which
      call the same natives with the same arguments as a real click and a real
      keystroke. It answered the text-drawing question in §1 that four earlier
      investigations recorded as unanswerable. Coordinates are separated by `x`,
      because a comma already separates timeline entries.
    - **Beware what else has focus.** A run under X11 owns the keyboard focus of
      the developer's session while its window is up, so anything they type
      lands in the field under test. One run here picked up a stray `5` that was
      not in the script and it is visible in the screenshot — read the
      `CORDIAL_TRACE_TEXT` log, not only the pixels.
  - **For another application, nest a compositor.** Run it under a headless
    `cage`/`weston` on its own `WAYLAND_DISPLAY`; a virtual-keyboard client bound
    to *that* compositor is global only within a compositor containing one
    window. Neither is installed on this machine as of 2026-08-02. `INFERRED` —
    the nesting approach is standard practice but has not been tried here.

## Measuring anything

- Use a **control**. The flag mechanism was only established by showing a log
  line vanishes with the flag set and is present without it, in the same session.
- **Repeat.** One bug here reproduced on roughly one launch in three and its rate
  moved with machine load, so before/after samples taken under different load are
  meaningless.
- Label anything you could not test **`INFERRED`**.
- **Know what your instrument costs.** `WAYLAND_DEBUG=1` produced three separate
  timing "findings" in one session that all evaporated when the same script was
  run without it (§1e). It is excellent for object identity and request order and
  worthless for anything with a clock in it.
- **A total is not a rate.** `vkQueuePresentKHR` counted over a fixed window on
  the landing page measures the engine's idle throttle far more than its frame
  rate — §1d has the curve. Sample per second, and say whether input was being
  delivered.
- **Prefer one long run to many short ones.** A launch puts a window on the
  owner's desktop. `CORDIAL_SCRIPT` (§1e) exists so that one `--run 240` can
  carry a whole matrix of conditions with its own controls inside it.

## A limit on the capture, stated honestly

Cordial runs natively on the host; the capture came from Waydroid. It is
trustworthy for **call order, names and contract** — which is what it was taken
for — and **not** for timing or render behaviour. Roblox under Waydroid burns CPU
with little GPU utilisation. Do not read its rendering path as a model of a
healthy one.

## On observing other binaries, including Sober

**Decompilation reconstructs expression.** You end up reading a reconstruction of
someone's source and writing code from it, which is where derivative-work risk
lives. That is why decompiled material is off limits (§16.1,
[ADR-001](adr/ADR-001-in-process-hooking.md)).

**A debugger on a running process yields behaviour** — which libraries load, which
natives are called, in what order, with what arguments. Facts and interfaces, not
expression.

So the line is not the tool, it is **what you take away**:

- Fine: call sequence, load order, argument shapes, resolved symbols, timing,
  syscalls.
- Not fine: stepping into its routines to read how it implements something and
  transcribing that logic. At that point the debugger is a slower decompiler.

**One rule, applied to any binary including Roblox: observe freely, do not
transcribe.** Sober was built by observing Roblox and nobody treats it as tainted
for it.

### What an attempt to trace Sober's text path established, and where it stopped

Attempted 2026-08-02. **No call sequence was captured** — the blocker was input
delivery, not the debugger. Recorded so nobody repeats the dead ends.

- **Sober loads `libroblox.so` the same "outside the system linker" way Cordial
  does.** No mapping in any process of its tree is named `libroblox`; the image
  lives in an unnamed `memfd`, and it is mapped **thirteen times** at different
  bases within one process, each an identically sized `r-xp`/`rw-p` pair. Why
  thirteen is not established and was not pursued — that is engine internals.
- **`LD_PRELOAD` interposition cannot work on these natives, in Sober or here.**
  Cordial's own `crates/cordial-linker-sys/src/lib.rs` resolves them through
  `cordial_linker_dlsym` — its *own* linker, off the ELF symbol table — and calls
  a raw pointer. The system dynamic linker is never asked to resolve
  `Java_com_roblox_engine_jni_NativeGLInterface_*`, so a shim shadowing that name
  is never consulted. The `memfd`/no-named-mapping result says Sober's loader
  does the equivalent. This is a dead end, not an untested idea.
- **The six text-path natives are exported and their offsets are one command
  away**, so the `0xCC` technique needs no preparation beyond a load base:
  ```bash
  readelf --dyn-syms -W lib/x86_64/libroblox.so | grep NativeGLInterface_
  ```
  Verified against the APK Sober had already downloaded. Offsets are not written
  down here on purpose: they change every build, and a stale table would be read
  as fact.
- **Sober's own binaries are stripped bare.** `nm -D` and `readelf --dyn-syms` on
  `/app/bin/sober` and `libloader.so` return zero dynamic symbols, no symtab, and
  `strings` finds no `showKeyboard`, `setSoftKeyboardActive`, `restartInput` or
  `gametextinput`. So the host→engine direction has no entry point reachable by
  name. The legitimate route is breakpointing `RegisterNatives`/`GetMethodID` and
  reading the `name`/`fnPtr` arguments off the call — recognising an unnamed
  function by its shape instead would be the forbidden move.
- **The remaining blocker was a keystroke**, and per the input rule above that
  meant either a human typing while breakpoints are already planted, or a nested
  headless compositor. `ptrace_scope` is 0, so attaching works; that step was
  simply never reached. For *Cordial* that blocker is gone (`CORDIAL_SCRIPT`'s
  `click:`/`type:`), but Sober is somebody else's window and the seam does not
  reach it, so this paragraph still stands for Sober.
- **A cheaper question first, and it needs no debugger.** Sober's engine writes
  the same `appData/logs/*_Player_*.log` Cordial's does, and the engine narrates
  `handleTextBoxFocused_AndroidLayer_` there at `FLog::NativeInput`. Focus a box
  in Sober, type, and grep. If Sober's engine hands the box to the same Android
  layer and still shows the text, §1's diagnosis is wrong; if it never reaches
  that line, the difference is upstream of drawing entirely. The one Sober log on
  this machine has zero `TextBox` hits, so nobody has checked.

---

# Solved, for reference

Kept because the *shape* recurs — most of these were ABI or contract mismatches
that presented as something else entirely.

| Symptom | Actual cause |
|---|---|
| Startup hung in a futex, then died | Asset folder passed as the unpack root. The engine wants `<root>/content` and resolves siblings against the parent, so `canonical` threw and `SingleSurfaceApp` aborted before instantiating its controllers |
| Every hostname failed to resolve | `struct addrinfo` has `ai_canonname`/`ai_addr` transposed between bionic and glibc, and the `AI_*` constants differ — bionic's `AI_DEFAULT` sets a bit glibc rejects with `EAI_BADFLAGS` |
| Every HTTPS request failed | Engine builds `./exe/cacert.pem` from a root it was never given; it now has its own run directory with the APK's CA bundle linked in |
| SIGSEGV on `HttpClient`, one launch in three | `realpath(path, NULL)` allocates with the host allocator; Roblox statically links mimalloc and freed a pointer its arena table never registered |
| `eglCreateWindowSurface` returned `EGL_BAD_ALLOC` | The engine was handed Cordial's `ANativeWindow*`; host EGL on X11 wants an XID |
| Vulkan refused to initialise | Roblox needs `VK_KHR_android_surface`, which desktop Mesa never exposes. Translated to `VK_KHR_xlib_surface` behind one interposed `vkGetInstanceProcAddr` |
| Engine reported "Android API 15" and refused Vulkan | `DeviceParams.osVersion` is read as an API *level*. Neither the system property nor `android_get_device_api_level()` fed it |
| Paths resolved against the working directory | `NativeSettingsInterface`'s directory setters were never called |
| GLES ran at about 1 fps while Vulkan was fine | Every `eglSwapBuffers` blocked ~1.00 s inside Mesa. The engine asks for vsync and Xwayland owns no CRTC, so DRI3's vblank query fell back to a one-second wait. The interval Mesa receives is now forced to 0; 20 → 652 swaps in 20 s |
| Interface looked like a low-end phone | Surface hardcoded to 720p and `dpiScale` to 1.0 — Roblox lays out in dp and picks asset resolutions from exactly those |

## Branches

`archive/gameactivity-per-callback` holds per-callback GameActivity dispatch that
was never merged. **Read it, do not merge it** — it is built on the disproved
ALooper theory and restructures the App Bridge onto a worker thread to fix a hang
whose real cause was the asset path. Merging it produced code that did not
compile against current main. One good idea in it: sharing one GameActivity
`thiz` and one Surface across calls, the way Android does.
