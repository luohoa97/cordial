# Desktop integration audit — what makes Cordial feel native, and what does not

**Status:** a read-only audit, 2026-08-27, of every file that decides how
Cordial presents itself to a Linux desktop before the engine is even involved:
the `.desktop` entry, the icon set, the AppStream metadata, the `GApplication`
wiring, and the deep-link path from a browser click to the engine's message
bus. Nothing here was built or run — a frame-rate measurement was in progress
on this machine at the time, and `AGENTS.md`'s rule against unverified claims
applies to a claim about *code shape* the same way it applies to a claim about
runtime behaviour, so every item below says which file it comes from rather
than asserting an outcome nobody watched.

`packaging/` and `.github/workflows/` were not edited while writing this — a
parallel agent has release-artifact work in flight there, and every one of the
files below that would need a change to close a gap lives in one of those two
directories. So this document does what it can from outside them: says
precisely what is already correct, and for what is not, says exactly what the
change would be without making it.

## 1. `roblox://` and `roblox-player://` deep links — already fully wired

This was the item most worth checking on the assumption something was
missing, because a `.desktop` entry that claims a URI scheme and a client that
drops what it is handed is worse than neither. It is not the case here. Four
layers, each one read directly rather than inferred:

1. **`packaging/io.github.luohoa97.Cordial.desktop`** declares
   `MimeType=x-scheme-handler/roblox-player;x-scheme-handler/roblox;` and
   `Exec=cordial-shell %u` — the `%u` is load-bearing, and the file's own
   comment records that an earlier version without it exported a handler that
   never received the URL it was invoked for.
2. **`crates/cordial-shell/src/main.rs`** registers the `GApplication` with
   `ApplicationFlags::HANDLES_OPEN | ApplicationFlags::HANDLES_COMMAND_LINE`
   and wires both `connect_command_line` and `connect_open`.
   `crates/cordial-shell/src/deep_link.rs` then validates whatever arrives —
   scheme, a 2048-byte cap, printable-ASCII-only — before it goes anywhere,
   and reads `argv` rather than the `GFile` the `open` signal hands over,
   because a test in the same file (`gio_reshapes_a_roblox_link_...`) pins
   down that GIO's `GFile::uri()` rewrites `roblox-player:1+…` into
   `roblox-player:///1+…` on this machine, which would have handed the client
   a corrupted link. The scheme list here (`SCHEMES` in `deep_link.rs`) is
   commented as having to stay in step with the `.desktop` file's `MimeType`
   line, and it currently does.
3. **`crates/cordial-runtime/src/deeplink.rs`** receives the validated link as
   `cordial-run --join-url <url>`, re-validates it (the same two schemes, the
   same cap, the same ASCII-only rule — deliberately duplicated rather than
   shared, because `cordial-shell` does not link the runtime), translates
   roblox.com's desktop-launcher link shape into the mobile shape the engine's
   own `FStringGameLaunchLinkURL` pattern matches, and publishes it on the
   engine's `MessageBus` as `Linking.detectURL`.
4. **This was measured against a running engine, not assumed.** The module's
   own doc comment records ten launches producing a `Game.launch` naming the
   place from the link, with `CORDIAL_DEEPLINK_NO_PUBLISH=1` as the control
   (same link, same launch, publish suppressed, `Game.launch` stays empty).
   `docs/analysis/deep-links.md` is the full protocol writeup this rests on.

**What is not established, stated as plainly as the module states it:**
whether a *join* actually succeeds end to end needs a signed-in account, and
every measured run ended at `app ready: Landing`, which is where a signed-out
client belongs. That is the module's own caveat, not a gap this audit found.

Nothing here needs a maintainer's attention. If anything changes the two
registered schemes, `deep_link.rs`'s `SCHEMES` constant, `deeplink.rs`'s
`SCHEMES` constant, and the `.desktop` file's `MimeType` line have to move
together — that discipline is already documented in both source files, which
is the right place for it to live.

## 2. The `.desktop` entry itself

Read in full at `packaging/io.github.luohoa97.Cordial.desktop`. Present and
correct: `Type=Application`, `Name`, `GenericName=Roblox Client`,
`Comment=Run Roblox on Linux`, `Icon=io.github.luohoa97.Cordial`,
`Terminal=false`, `Categories=Game;`, `StartupNotify=true`,
`StartupWMClass=Cordial`, and the `MimeType` line from §1.

Two things worth knowing that are easy to assume are bugs and are not:

- **`Exec=cordial-shell %u`, not `cordial %u`.** The file's own comment
  explains this is deliberate — nothing is installed as plain `cordial`, and a
  shortened `Exec` line produced `bwrap: execvp cordial: No such file or
  directory` on every click before this was fixed. If this reads like a typo
  in a future pass, it is not one.
- **`StartupWMClass=Cordial` is enforced by a compiled-in test**, not just a
  comment: `crates/cordial-shell/src/host_window.rs` has
  `app_id_matches_the_desktop_entry`, which `include_str!`s the `.desktop`
  file at compile time and asserts its `StartupWMClass` equals the
  `host_window::APP_ID` constant GTK actually sets on the toplevel. A drift
  between the two — which is exactly the kind of thing that breaks window
  matching, taskbar grouping and `wmctrl`-style tooling silently — fails the
  test suite rather than shipping.

**One real, small gap: no `Keywords=` line.** GNOME Shell's and KDE's
application search both weight `Keywords` alongside `Name`/`GenericName`/
`Comment`, and searching "roblox" already matches here via `GenericName`, so
this is not a case of the app being unfindable — it is a case of it being
findable by fewer terms than it could be. A line such as
`Keywords=Roblox;Metaverse;Game;Sober;` (Sober included on purpose: someone
searching for the thing they already know the name of should find the thing
that plays the same game) would cost nothing and is a one-line change to a
file this audit could not touch. Left for the maintainer rather than guessed
at further, because the right keyword set is a product question, not a
technical one.

**No `Actions=` group.** A right-click quick action ("New profile", "Settings")
is a nicety GNOME and KDE both support and Cordial does not use. Not a defect —
most single-window applications do not need one — but worth naming as an
option rather than an oversight nobody considered.

## 3. Icons

`packaging/icons/hicolor/scalable/apps/` carries two SVGs, both installed by
every packaging path checked (`packaging/io.github.luohoa97.Cordial.yml`,
`packaging/aur/cordial-git/PKGBUILD`, and referenced identically in
`packaging/rpm/cordial.spec`'s comments): `io.github.luohoa97.Cordial.svg`, the
real icon, and `io.github.luohoa97.Cordial.Frostbite.svg`, the twice-a-year
alternate `crates/cordial-shell/src/branding.rs` switches to — installed
unconditionally, with the reasoning recorded in the Flatpak manifest's own
comment, because Flatpak only exports files whose names begin with the app ID
and a name resolving to nothing on the one day nobody is watching for it is
worse than shipping it always.

Both are scalable SVG only. That is sufficient for GTK4/libadwaita and current
KDE Plasma, which both rasterise `hicolor/scalable/apps` SVGs at whatever size
they need — this is not the same situation as a toolkit that only reads fixed
PNG sizes from the icon cache, which is what the historical advice to ship
16/32/48/64/128/256 PNGs alongside the SVG was for.

**One real gap: no symbolic variant.** A `io.github.luohoa97.Cordial-symbolic.svg`
(single-colour, following the standard "symbolic" style GNOME's HIG documents)
is what GNOME Shell prefers for notification icons and what a high-contrast
theme substitutes for the full-colour icon in some contexts. Its absence is
not a defect today — Cordial does not currently send desktop notifications
that would show one — but it is the kind of small polish item Flathub's own
quality guidelines call out, and worth having whenever the icon set is next
touched. Not fixed here because `packaging/icons/` is out of reach for this
change and a symbolic icon is a design artefact, not a config edit — it wants
an actual drawing, not a generated stand-in.

## 4. MIME registration beyond the two URI schemes

Cordial registers no file-extension MIME type, and that is correct rather than
missing. The one file type a user interacts with directly — a plugin's
`.tar.zst` archive — is picked through a `GtkFileDialog` in
`crates/cordial-shell/src/settings.rs` (`choose_file`), which is a deliberate
act inside Settings rather than a double-click-to-install flow, and
`docs/adr/ADR-014-plugin-registry-and-unpacking.md` is where that shape is
decided. Registering `.tar.zst` as "opens with Cordial" would also collide
with every other tool on a desktop that already claims that extension for
plain archives. Nothing to change here.

## 5. GameMode, and other things already defaulted away correctly

`crates/cordial-shell/src/shell_config.rs`'s `gamemode` field is **default
on**, with the comment stating why in exactly the terms this audit's brief
asked about: "a performance setting nobody finds is a performance setting
nobody gets," and "it costs nothing on a machine without gamemoded — the
request is a D-Bus call that fails, the client says so once and carries on."
`false` here is what becomes `CORDIAL_GAMEMODE=0`, so the mechanism is opt-out
rather than opt-in. This is the shape every other item in this section is
being measured against, and it already exists.

Sign-in is Quick Sign-in, a code flow needing no typing (README's status
table, corroborated by the presence of `crates/cordial-runtime/src/cookies.rs`
persisting the session to the desktop keyring rather than a file). Profile
choice is a chooser above the Launch button that creates one if none exists.
Neither asks for configuration a first-time user would have to look up.

## 6. First run: the one real friction point, already root-caused, not yet resolved

**The rendering surface defaults to 720p at `dpiScale` 1.0** — README §5 and
`docs/NEXT.md`'s "Solved, for reference" table both record this, the latter
under "Interface looked like a low-end phone: Surface hardcoded to 720p and
`dpiScale` to 1.0 — Roblox lays out in dp and picks asset resolutions from
exactly those." That is a phone's density, not a desktop monitor's, and fixing
it today needs two environment variables set outside the application —
`CORDIAL_RESOLUTION` and `CORDIAL_DPI_SCALE` — read from `grep` across
`crates/cordial-shell/src/settings.rs`, which has no UI control for either.

**This is a real "press one button and play" gap** — a first launch renders
at a size and density nobody would choose, and the fix is not discoverable
from inside the running application, only from the README. It is also
**already understood and already deliberately left as an env-var knob rather
than a changed default**, per the "Solved, for reference" table's own framing:
the root cause is known, and the entry does not say the default is wrong, only
that it explains a symptom. Changing the shipped default, or adding a Settings
control for resolution and density, touches asset selection — "Roblox lays out
in dp and picks asset resolutions from exactly those" is the same sentence
that explains why 720p/1.0 looks like a phone and why picking a new default
blind is not obviously safe. That is a product and correctness judgement, not
a config file this audit can respond to, and this project's own rule is that a
timing or behavioural claim needs to be measured rather than assumed — which a
new default would, on a screen, after a build. **Flagged for the maintainer
rather than changed here**, precisely because closing it responsibly means
running the client at a new default and looking at it, which this session was
asked not to do.

**Resolved 2026-11-08 (PR #144).** Wayland now creates the canvas at the output's
physical scale and settles `DisplayMetrics`, `AConfiguration` and the dp size from
the same output scale before `initializeNativeCode`; a 2× display gets a 2×
framebuffer at the same interface size, with no environment variables. The
720p/`dpiScale` 1.0 default described above is now only what X11 and a client with
no window scale still get.

## Summary for the maintainer

| Item | State | Needs |
|---|---|---|
| `roblox://` / `roblox-player://` deep links | Fully wired, measured against a running engine | Nothing |
| `.desktop` core fields (Name, Icon, Categories, StartupWMClass, MimeType) | Correct, one field enforced by a compiled-in test | Nothing |
| `.desktop` `Keywords=` | Absent | A one-line addition once packaging/ is free to edit |
| `.desktop` `Actions=` | Absent | Optional; a product decision, not a gap |
| Icons — full-colour SVG at every size (scalable) | Present, correctly installed everywhere packaging looks for it | Nothing |
| Icons — symbolic variant | Absent | A drawing, not a config edit |
| MIME registration for plugin archives | Deliberately absent, handled via file picker | Nothing |
| GameMode, Quick Sign-in, profile auto-creation | Already defaulted on / frictionless | Nothing |
| Render resolution / DPI-scale default | Root-caused, still phone-shaped by default | A maintainer decision, made while watching a build render — not something to change unverified |
