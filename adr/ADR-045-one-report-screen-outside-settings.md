---
title: "ADR-045: One report screen, outside Settings, and a notice on X11"
---
**Status:** accepted
**Date:** 2026-09-30
**Related:** [ADR-011](/adr/ADR-011-wayland-and-libadwaita), [ADR-024](/adr/ADR-024-x11-is-supported-again), [ADR-030](/adr/ADR-030-reports-arrive-from-discord)

## Decision

1. **The diagnostics block lives on one screen, `report.rs`, titled "Report a
   Problem".** It has the block, Copy, Save to a file, and a link to the issue
   form. It replaces Settings' Report page and the About dialog's
   Troubleshooting page, which showed the same `diagnostics::report()` text
   and were found separately or not at all.
2. **It is an `AdwDialog` of its own, not the About dialog's Troubleshooting
   page.** `AdwAboutDialog` cannot be opened on a chosen page, and the
   launcher's X11 notice has to land exactly here. The About dialog no longer
   sets `debug_info`. Its "Report an Issue" row is intercepted with
   `activate-link` and opens this screen instead of the web form; the row
   keeps libadwaita's external-link icon, which is not quite true.
3. **Settings holds settings.** Its pages are Roblox, Updates, Version, General,
   Plugins and FastFlags. Report was the one page that was not a setting.
   The entry points are the main menu's "Report a Problem", the About dialog,
   and the launcher notice.
4. **The launcher shows a notice when the game will open on X11.** An
   `AdwBanner`: "X11 support is buggy. If something goes wrong, try Wayland."
   with a Report a Problem button. "Will open on X11" is the loader's own rule,
   `cordial_runtime::android::backend()`: `CORDIAL_X11` set, or no
   `WAYLAND_DISPLAY`. `x11_notice::uses_x11` repeats it over the same two
   variables, and the two must be changed together. (Since the doctor states
   the same rule, `x11_notice::uses_x11` calls `doctor::uses_x11` in the
   library; the loader's copy is the one left to keep in step.) It is deliberately not "the
   launcher's GDK display is X11": a session exporting `GDK_BACKEND=x11` over a
   Wayland compositor runs the game on Wayland.

## Rejected

**A link inside the banner text.** `AdwBanner` exposes no `activate-link`, so a
link would have opened a web page or needed a walk of the banner's internal
widgets. The banner's button is the libadwaita way to attach an action.

## Not verified

Under real X11 the launcher was not photographed: Xwayland would not start in
the nested compositor used here (`/tmp/.X11-unix` not owned by the user inside
the container). The banner was shown with `CORDIAL_X11=1` on Wayland, which
takes the same branch; the no-`WAYLAND_DISPLAY` branch is covered by a unit test
of `uses_x11` only.

## Added 2026-09-30

The screen also carries the doctor's checks under "This machine", and Copy and
Save include them. They are asked on a worker, without the network, so the
screen opens at once; the text Copy hands over is the block alone until they
finish. See [`docs/doctor.md`](/doctor).

## Added 2026-10-08: Save logs

The screen has a **Save logs…** row beside Save to a file, and the "Open an
issue" row asks whether to save the logs before it opens the form. Both write
one `.zip` ([`log_export.rs`](https://github.com/luohoa97/cordial/blob/main/crates/cordial-shell/src/log_export.rs)):
the diagnostics and checks, the launcher's own output this session, the last
client run's output and command line, the last `[cordial] health:` lines, the
newest `*_last.log` of the active profile's phone and Quest builds (each cut to
its last 2 MiB), and a note on whether `coredumpctl` knows a dump of
`cordial-run`. No core file is ever included: it holds the client's memory,
session cookie included.

- **The launcher never kept a log.** `session_log.rs` now tees the launcher's
  stdout and stderr through pipes into a 3,000-line in-memory ring, and keeps
  the last client's tail buffer (the one the crash page reads) and a health
  ring. Nothing is written to disk until the user saves. `CORDIAL_NO_LOG_CAPTURE=1`
  turns the tee off. A cost: the two streams stop being terminals, so GLib
  stops colouring its warnings.
- **Redaction happens at export, over all sources at once**, so an id first
  seen on the engine's join line is the same `<user-id-1>` everywhere. Rules
  are in the module header: session and chat lines, learned ids and names,
  secret/id/name/text keys, URL queries, `_|WARNING` values, home and profile
  names. It is pattern matching and says so in the archive's README; the
  screen says the files are redacted, not that they are safe.
- **`.zip`, not `.tar.gz`**, because GitHub accepts a `.zip` as an attachment
  and no new dependency is needed.
- Why before the form: it opens in a browser, so there is no later moment at
  which this window can tell the user to attach something.

**Not verified:** the redaction against every line a real session prints. It is
tested on the shapes the repository's own fixtures use; a secret in a shape
nobody has seen will get through.
