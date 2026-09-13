<p align="center">
  <img src="https://raw.githubusercontent.com/luohoa97/cordial/main/packaging/banner.svg" alt="Cordial" width="460">
</p>

# Open-source Roblox for Linux — run it natively, extend it yourself

<p align="center">
  <a href="https://discord.gg/qJzU3Xfr9b">
    <img src="https://img.shields.io/badge/Discord-join%20the%20server-5865F2?style=for-the-badge&logo=discord&logoColor=white"
         alt="Join the Cordial Discord">
  </a>
</p>

<p align="center">
  <strong><a href="https://discord.gg/qJzU3Xfr9b">Come and talk to us on Discord</a></strong> for help getting
  it running and what is being worked on. Bugs and feature requests go on
  <a href="https://github.com/luohoa97/cordial/issues/new/choose">GitHub</a>, not in chat, so they don't get lost.
</p>

<p align="center">
  <img src="https://raw.githubusercontent.com/luohoa97/cordial/main/docs/media/cordial-doors.gif"
       alt="Roblox DOORS running under Cordial on Linux: first-person corridor, candle in hand"
       width="560">
</p>

<p align="center">
  <em>Roblox <strong>DOORS</strong>, unmodified, on Cordial — Fedora, GNOME, no Android device involved.<br>
  <a href="https://raw.githubusercontent.com/luohoa97/cordial/main/docs/media/cordial-doors.mp4">This clip at full size</a>,
  and more in <a href="docs/media">docs/media</a> — including an hour of Rivals cut down to its eliminations.</em>
</p>

*A hobby project, not a commercial one. Please don't DMCA it.*

## Get it running

```bash
flatpak remote-add --if-not-exists cordial https://luohoa97.github.io/cordial/cordial.flatpakrepo
flatpak install cordial io.github.luohoa97.Cordial
flatpak run io.github.luohoa97.Cordial
```

Or take the AppImage from [the releases
page](https://github.com/luohoa97/cordial/releases), which installs nothing and
runs anywhere: `chmod +x Cordial-x86_64.AppImage && ./Cordial-x86_64.AppImage`.
[Install](#install) compares the two and says what is less proven about the
newer one.

**You also need Roblox's Android build, which Cordial does not ship and never
will.** First run has one button — **Download Roblox** — and that is the whole
procedure. Cordial fetches the build from APKPure, a third-party mirror, and
**refuses to install anything that is not signed by Roblox's own signing
certificate**, so a mirror that alters a byte is caught rather than trusted. It
waits for your press before downloading, since this is a few hundred megabytes
and somebody may be paying for it by the megabyte.

You never have to press it if a build is already on the machine:

- **Already have [Sober](https://sober.vinegarhq.org/)?** Then there is nothing
  to press. Cordial finds the APK Sober downloaded and uses it where it lies —
  no copy, no modification, and Sober keeps working.
- **Supply your own APK** and point Cordial at it in Settings, or see
  [What you need](docs/install.md#what-you-need). It gets the same signature check.

Two things worth knowing before you type that, rather than after: **the remote
is not signed**, so `flatpak install` proves the download matches the
repository's checksums and nothing about who built it — the
[full explanation](docs/install.md#trust-and-what-not-signed-means) is in
`docs/install.md` and you should read it. And Cordial is experimental:
sign-in, gameplay, mouse and keyboard, text entry and audio all work; the
[status page](docs/status.md) says exactly what does not.

Cordial loads Roblox's official Android x86-64 engine directly on Linux through a
purpose-built runtime: the AOSP bionic linker, a bionic/glibc shim, a JNI VM in
place of Android's, and a framework layer that answers the client's calls. No
emulator, no container, no virtual machine. It talks to your GPU through Vulkan
or GLES2 the way any native application does.

**It is also, as far as we know, the first user-extensible Roblox client** — not
in the sense of replacing files or setting flags, which other launchers already
do, but in the sense that *you can write code that runs as part of the client*.
Plugins are ordinary programs in their own processes with named capabilities
rather than access, and Cordial's own default features are built as plugins too,
so the API has to be good enough for them. Browser extensions only reach
Roblox's **website**, launcher mods replace **assets**, and FastFlag managers
change **settings Roblox already reads** — none load user-written code into the
client itself; if one already does, we would genuinely like to know.

What this is **not** is a way to modify Roblox itself. There is no script
execution, no hooking, and no memory access — absent from the API rather than
disabled. Plugins extend *Cordial*.

## Get started

- [Join the Discord 💬](https://discord.gg/qJzU3Xfr9b)
- [Read the documentation 📖](docs)
- [Start here — what works and what is blocking 🧭](docs/NEXT.md)
- [Install it 🔽](#install)
- [How it actually works 🔬](docs/findings.md)
- [Why there is no script execution, ever 🔒](docs/adr/ADR-001-in-process-hooking.md)
- [Report a bug or suggest a feature 🐛](https://github.com/luohoa97/cordial/issues/new/choose)
- [Contribute 🛠️](CONTRIBUTING.md)

**New here?** Read the warning below first, then
[`docs/NEXT.md`](docs/NEXT.md) — it is written for someone picking the project
up cold and says plainly what is broken and what has already been ruled out.

## Reporting a problem

[GitHub Issues](https://github.com/luohoa97/cordial/issues/new/choose) is
where a bug, a broken Roblox feature, a failed update, a feature suggestion,
or a finding goes — not Discord, which is faster for a quick question but does
not get triaged and is not searchable later. Blank issues are off; pick the
template that matches and it asks for the right things — see
[`.github/SUPPORT.md`](.github/SUPPORT.md) for the full list. Security issues
go through [a private advisory](https://github.com/luohoa97/cordial/security/advisories/new)
instead of a public one; see [`SECURITY.md`](SECURITY.md).

**Every template requires a Diagnostics block.** Get it from
**Settings → Report a Problem**, which has a copy button, or from a terminal:

```bash
cordial --diagnostics                                   # .deb / .rpm / Arch
flatpak run io.github.luohoa97.Cordial --diagnostics    # Flatpak
./Cordial-*.AppImage --diagnostics                      # AppImage
```

It carries the Cordial and Roblox build, `uname -a`, your distribution and
package format — the things a report here is usually missing. **It does not
carry your account, any token, your profile name, or any path under your home
directory**, though it does carry your machine's hostname, shown on screen
before it is copied so you can edit it out.

> ### ⚠️ Read this before using an account you care about
>
> **This is NOT an official Roblox client**, and it is not endorsed or sponsored
> by Roblox Corporation, which has not approved this project and has not been
> asked to. Roblox does not support third-party clients and operates automated
> systems that ban accounts for using them, up to permanent termination —
> including false positives against innocent players, in waves, associating
> accounts that share an IP address.
>
> Cordial does not modify the Roblox client or exploit it — no script executor,
> no hooking, no memory access to the Roblox process, absent from the API
> rather than disabled ([ADR-001](docs/adr/ADR-001-in-process-hooking.md),
> [ADR-003](docs/adr/ADR-003-plugin-isolation.md)) — but it necessarily presents
> a synthesised Android environment, and a heuristic detector does not owe you
> that distinction. Alternate accounts are not a shield.
>
> **If your account matters to you, do not use it here.** If you use Cordial and
> get banned, that is on you, and the maintainers cannot get it reversed. See
> [CONTRIBUTING.md](CONTRIBUTING.md) for testing with a throwaway account on its
> own IP.

## Status: experimental, but playable

The full feature table, what changed recently in this fork, and three of the
harder bugs it took to get here — the content store, the keyboard, and running
two accounts at once — are in [`docs/status.md`](docs/status.md). Read it
before installing.

## Install

> Cordial is experimental. [`docs/status.md`](docs/status.md) says what works
> today — read it before installing.

**x86-64 Linux, Wayland.** X11 still starts via Flatpak's fallback socket but
is not developed further ([ADR-011](docs/adr/ADR-011-wayland-and-libadwaita.md)).
You also need Roblox's Android build, which the **Download Roblox** button on
first run fetches for you — see [`docs/install.md`](docs/install.md) for the
Sober and custom-APK routes.

**Flatpak**, sandboxed and self-updating — pick this unless you have a reason
not to:

```bash
flatpak remote-add --if-not-exists cordial \
    https://luohoa97.github.io/cordial/cordial.flatpakrepo
flatpak install cordial io.github.luohoa97.Cordial
flatpak run io.github.luohoa97.Cordial
```

**AppImage**, one file, no install, updates manually — newer and less proven,
see [`docs/install.md`](docs/install.md#appimage):

```bash
chmod +x Cordial-x86_64.AppImage && ./Cordial-x86_64.AppImage
```

**APT** (Debian/Ubuntu):

```bash
sudo apt install ./cordial_*_amd64.deb   # from the releases page
```

**dnf** (Fedora 44 today, RHEL and derivatives):

```bash
sudo dnf install ./cordial-*.x86_64.rpm  # from the releases page
```

**pacman** (Arch and derivatives):

```bash
sudo pacman -U cordial-*-x86_64.pkg.tar.zst  # from the releases page
```

All four release artefacts are cosign-signed; the Flatpak/APT/dnf/pacman
*repositories* are not, yet, and the AUR is blocked on account sign-ups being
closed. Verifying a signature, repository status and key fingerprints:
[`docs/install.md`](docs/install.md).

**Building from source:**

```bash
git clone --recursive https://github.com/luohoa97/cordial
cd cordial
cargo build --release
```

Needs Clang (AOSP bionic uses C11 `_Atomic` in C++ headers; GCC rejects it)
and GTK4 ≥ 4.10 / libadwaita ≥ 1.4 development packages. Full dependency
list and the Flatpak build: [`docs/install.md`](docs/install.md#building-from-source).

**Running a source build:**

```bash
cargo run --release --bin cordial-run -- \
  --lib-dir /path/to/lib/x86_64 --apk /path/to/base.apk \
  --host-libc --game-activity --run 30
```

`--run` is how many seconds to stay up; `cordial-run --help` lists the rest.

### Useful knobs

| | |
|---|---|
| `CORDIAL_MONITOR=<n>` | open on the nth monitor instead of the primary one |
| `CORDIAL_FULLSCREEN=1` | cover that monitor |
| `CORDIAL_RESOLUTION=<w>x<h>` | render resolution, default 1280x720 |
| `CORDIAL_DPI_SCALE=<f>` | UI density Roblox lays out against; 1.0 is a low-density phone |
| `CORDIAL_NO_POINTER_LOCK=1` | never capture the cursor at all |
| `CORDIAL_PRESENT_MODE=<m>` | frame pacing: `mailbox` (default), `fifo`, `immediate`, `uncapped`, `off` — Settings has a row for this |
| `CORDIAL_GAMEPAD_TYPE=<n>` | which controller brand Roblox draws glyphs for — see [`docs/controllers.md`](docs/controllers.md) |
| `CORDIAL_ANDROID_TRACE=1` | log Android API calls |
| `CORDIAL_COUNT_GL=1` | report graphics calls on exit |

FastFlag overrides live in
`~/.local/share/cordial/profiles/<profile>/flags.json` — syntax, layering and
gotchas are in [`docs/fastflags.md`](docs/fastflags.md).

### When something goes wrong

**Read the engine's own log first.** Roblox writes it to
`<files>/appData/logs/*.log` and it names subsystems, stages, paths and
exceptions in its own words — most questions are answered by the newest file
there. To check whether input is reaching the engine, run with
`CORDIAL_ANDROID_TRACE=1` and look for `onTouchEventNative(...) -> true`. If
none of that explains it, [file an issue](#reporting-a-problem).

## FAQ

**Is this an official Roblox client?** No. Not affiliated with, endorsed by, or
sponsored by Roblox Corporation. It runs Roblox's own Android build under a
runtime this project wrote.

**Will my account get banned?** Possibly — read the warning under
[Reporting a problem](#reporting-a-problem). Use a throwaway account on its own
IP if you are testing.

**Why isn't Cordial on Flathub?** Flathub's generative-AI policy excludes it —
Cordial's commit history is honest about being AI-assisted, and that puts it on
the wrong side of the policy. Detail: [`docs/install.md`](docs/install.md).

**Why isn't there an AUR package yet?** The PKGBUILDs are ready; AUR account
sign-ups are currently closed. Install the release `.pkg.tar.zst` instead.

**Can two accounts run at once?** Yes — two profiles, two instances, side by
side, each about 1.5 GB. [`docs/status.md`](docs/status.md).

**How do I change FastFlags or the graphics backend?**
[`docs/fastflags.md`](docs/fastflags.md).

**My controller shows the wrong button icons.** A known, unsolved mapping
problem — every button still works. [`docs/controllers.md`](docs/controllers.md).

**Can I install a plugin someone sent me?** Yes, from a `.tar.zst` archive via
**Settings → Get Plugins** — there is no plugin store yet.
[`docs/plugins.md`](docs/plugins.md).

**Something's broken.** Read the engine's log first (see
[When something goes wrong](#when-something-goes-wrong)), then
[file an issue](#reporting-a-problem) with a Diagnostics block.

## Plugins

**Three ship with Cordial and you already have them.** Open Settings and go to
Plugins; they are listed there whether or not you have ever installed anything.

| | What it does | On by default |
|---|---|---|
| **FPS Flex** | Stops drawing being pinned to your display's refresh — the same lever as **Frame pacing** in Settings, not a second one. Off by default: uncapping presentation makes your GPU draw frames nobody asked for, which on a laptop is heat and battery. | No |
| **Discord Presence** | Shows on your Discord profile what you are playing. Ships but is not yet wired to a running client — [`docs/rich-presence.md`](docs/rich-presence.md). | No |
| **Flag Inspector** | Logs which FastFlags are in effect and where each came from. A diagnostic, not a feature. | No |

Nothing runs until you enable it, and a plugin only gets the permissions you
approve, per profile — approving something in a test profile does not approve
it in the one you actually play on.

Plugins run on [Deno](https://deno.com)
([ADR-008](docs/adr/ADR-008-plugins-are-typescript-on-deno.md)); Cordial offers
to fetch a pinned copy if none is on `PATH`. Installing one you were sent, and
writing your own: [`docs/plugins.md`](docs/plugins.md),
[`plugins/README.md`](plugins/README.md); the capability model is
[ADR-007](docs/adr/ADR-007-host-resources-are-brokered.md).

## Documentation

Start with [`docs/NEXT.md`](docs/NEXT.md) — written for someone picking the
project up cold, and plain about what is broken. The full index of design
notes, ADRs and analyses is [`docs/README.md`](docs/README.md);
[`CHANGELOG.md`](CHANGELOG.md) and the
[releases](https://github.com/luohoa97/cordial/releases) page have what
changed between versions.

## What this is built on, and who it is owed to

**Sober**, VinegarHQ's client, is the reason anyone believes a Roblox client
can run natively on Linux, and Cordial owes it three specific debts: its
[issue tracker](tools/sober-corpus/) is a research corpus read before
investigating any user-facing bug ([ADR-017](docs/adr/ADR-017-sober-issue-corpus.md));
watching it run corrected a claim made here about text input
([`docs/analysis/sober-input-stack.md`](docs/analysis/sober-input-stack.md));
and it was how everybody here got a Roblox build before Cordial could fetch
its own. **Sober's code was never read** — it is not source-available, and
what was used is a public issue tracker and the observable behaviour of a
running program, the same class of evidence as watching any program work.

**mocktail**, komaruworld's client, is Apache-2.0; where its ideas are adapted
they are credited in [`NOTICE`](NOTICE) and named at the point of use — the web
view's security rules are theirs. **AGDK `GameActivity`** is also Apache-2.0,
which is why the activity, surface, input and IME contract could be read
rather than guessed at.

## Headline findings

- Roblox ships a complete x86-64 Android build — `split_config.x86_64.apk`
  carries 116 MB of x86-64 machine code (NDK r28c), so Cordial needs no CPU
  architecture translation, only feature emulation.
- The runtime surface is bounded: 13 Android libraries linked, 644 undefined
  symbols, GLES2 + EGL mandatory with Vulkan `dlopen`ed as an optional upgrade.
- Roblox's game surface is AGDK `GameActivity`, Apache-2.0, so it could be read
  rather than inferred.

Full analysis: [`docs/findings.md`](docs/findings.md).

## Not in scope, permanently

No in-process code execution against the Roblox process: no hooking, no memory
patching, no injected script environment. Not "disabled by default" — absent from
the API vocabulary, so there is no injection primitive in the binary to extract.
Reasoning in [ADR-001](docs/adr/ADR-001-in-process-hooking.md).

Also out: client-side integrity flags or watermarks, and obfuscation-as-security.

## Star History

<a href="https://www.star-history.com/?repos=luohoa97%2Fcordial&type=date&legend=top-left">
 <picture>
   <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/chart?repos=luohoa97/cordial&type=date&theme=dark&legend=top-left&sealed_token=k2BpUmlDBarFv8DEaibONMzIVqR354Y0p6GxcrH9umRfO7ofVa2KNYn9t5BypPU7oGyVHGS8s0wnGiRbLNDvNDI2nYv9wRglmTifqAQZ0fBdsKEKT6d6K9S4QIFhx3VwlQzJOrjE0yCpaHWX23qzsM4zS7CE4ted0uz1KxgK4fW7eZLA-NRhPifkQPqL" />
   <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/chart?repos=luohoa97/cordial&type=date&legend=top-left&sealed_token=k2BpUmlDBarFv8DEaibONMzIVqR354Y0p6GxcrH9umRfO7ofVa2KNYn9t5BypPU7oGyVHGS8s0wnGiRbLNDvNDI2nYv9wRglmTifqAQZ0fBdsKEKT6d6K9S4QIFhx3VwlQzJOrjE0yCpaHWX23qzsM4zS7CE4ted0uz1KxgK4fW7eZLA-NRhPifkQPqL" />
   <img alt="Star History Chart" src="https://api.star-history.com/chart?repos=luohoa97/cordial&type=date&legend=top-left&sealed_token=k2BpUmlDBarFv8DEaibONMzIVqR354Y0p6GxcrH9umRfO7ofVa2KNYn9t5BypPU7oGyVHGS8s0wnGiRbLNDvNDI2nYv9wRglmTifqAQZ0fBdsKEKT6d6K9S4QIFhx3VwlQzJOrjE0yCpaHWX23qzsM4zS7CE4ted0uz1KxgK4fW7eZLA-NRhPifkQPqL" />
 </picture>
</a>

## Licence

GPL-3.0-or-later. See [`LICENSE`](LICENSE).

Third-party components keep their own licences and notices, reproduced in
[`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md) and installed alongside the
binary by the Flatpak:

- [`third_party/libbadcpu`](third_party/libbadcpu) — MIT, vendored from
  [Sober OSS](https://github.com/Z3ki/sober-oss)
- `mcpelauncher-linker` — MIT, ChristopherHX and MCMrARM
- AOSP bionic, carried within it — Apache-2.0 and BSD
- `libjnivm` — MIT, ChristopherHX

MIT and Apache-2.0 are satisfied while the combined work is offered under the
GPL, provided those notices travel with it. That is a condition, not a
courtesy.
