# Installing Cordial

The README has the quickstart commands. This is the detail behind them: what
each package format is for, the honest state of the AppImage's web view, and
how the release signatures and repository trust actually work.

## What you need

- x86-64 Linux
- A Wayland session. X11 still starts, through Flatpak's fallback socket, but
  [ADR-011](adr/ADR-011-wayland-and-libadwaita.md) makes Wayland the
  backend Cordial targets and says X11 is not developed further
- Roblox's official Android client, which **you supply** — Cordial ships no
  Roblox code, APK or assets and never will

From an installed APK you need the `lib/x86_64/` objects and the base APK.

**The shortest route to one is the Download Roblox button**, which fetches and
verifies a build without you leaving Cordial. That is new; it used to be
"install Sober first", and that answer still works.

[Sober](https://sober.vinegarhq.org/) downloads Roblox's Android build for its
own use, and Cordial still looks for it there —
`~/.var/app/org.vinegarhq.Sober/data/sober/packages/x86_64/`. Nothing is copied
and nothing is modified; Cordial reads the APK where it already is. If you have
Sober, Cordial finds its build and never asks you for one. You are free to keep
using Sober afterwards, or not.

If you have an APK of your own, Settings takes a path to it, and `--apk` takes
one on the command line. On a split build the engine is in
`split_config.x86_64.apk` rather than `base.apk`; Cordial checks the siblings
itself and says which it tried when it cannot find one.

Nothing else. The Flatpak carries the toolchain and the libraries with it; the
build dependencies are under [Building from source](#building-from-source) below.

## Flatpak vs AppImage

Building from source is for people changing Cordial, not for people running
it — see below.

**Flatpak is the one to pick if you have no reason to prefer the other.** It is
sandboxed, it updates in place, and the manifest is the reference every other
package here is built to match.

```bash
flatpak remote-add --if-not-exists cordial \
    https://luohoa97.github.io/cordial/cordial.flatpakrepo
flatpak install cordial io.github.luohoa97.Cordial
```

Then launch Cordial from your desktop's application list, or
`flatpak run io.github.luohoa97.Cordial`. `flatpak update` picks up new builds.
Uninstall with `flatpak uninstall io.github.luohoa97.Cordial`, and
`flatpak uninstall --delete-data io.github.luohoa97.Cordial` if you also want the
profiles, the sign-in and the extracted Roblox build gone.

**The AppImage is one file that runs on any distribution.** No remote to add,
no package manager, nothing installed system-wide — download `Cordial-x86_64.AppImage`
from [the releases page](https://github.com/luohoa97/cordial/releases), then:

```bash
chmod +x Cordial-x86_64.AppImage
./Cordial-x86_64.AppImage
```

It carries GTK4, libadwaita and WebKitGTK with it, so it does not care what
your distribution ships. It installs nothing; delete the file and Cordial is
gone, though your profiles stay in `~/.local/share` until you remove them
yourself. It needs FUSE, which nearly every desktop has; if it refuses to
start, run it with `--appimage-extract-and-run` and it will unpack to a
temporary directory instead. Updates are manual — the Flatpak updates itself,
which is the main practical reason to prefer it. The AppImage is newer and
less proven; read the rest of this section before choosing it.

### AppImage

**The web view, and what is still not established.** WebKitGTK does not link
the processes that draw a page. It spawns `WebKitWebProcess` and
`WebKitNetworkProcess`, loads an injected bundle, and runs `bwrap` and
`xdg-dbus-proxy` for its own sandbox — five things reached through absolute
paths fixed when WebKitGTK itself was built, `/usr/libexec/webkitgtk-6.0` on
Fedora and somewhere different on every other distribution. Up to and including
v0.13.0 the AppImage carried copies of them and nothing made WebKitGTK look at
the copies, so on a host that had never installed WebKitGTK the sign-in window
came up blank and the log said `Failed to spawn child process
".../WebKitNetworkProcess"`. Installing WebKitGTK did not help unless you were
on Fedora, because nobody else uses that path.

Cordial now makes those paths resolve to its own copies inside a private mount
namespace, which needs `bwrap` and unprivileged overlay mounts. If your kernel
or distribution refuses either, the AppImage says so on standard error and
carries on without them, and the web view then needs WebKitGTK 6.0 installed at
Fedora's path. **This has been measured on a stand-in for a machine with no
WebKitGTK, but not yet on a real one, and not on any distribution other than
Fedora** — if the sign-in window is blank, please report it with whatever the
terminal printed rather than assuming Cordial is broken. The Flatpak is
unaffected either way.

## APT (Debian/Ubuntu)

**The `.deb` on the release page installs today and needs no repository.**
Every release attaches one, with a `.cosign.bundle` beside it:

```bash
# From https://github.com/luohoa97/cordial/releases/latest
sudo apt install ./cordial_*_amd64.deb
```

Verifying it first is worth the two commands — see
[Verifying a release download](#verifying-a-release-download). The repository
below is the nicer route once it is up, and **it is not up yet**: it publishes
nothing until a maintainer adds a signing key, so following these commands
today gets you a 404 rather than Cordial.

Cordial's own repository, not a package in Debian or Ubuntu itself — see
[`docs/design/apt-repository.md`](design/apt-repository.md) for why
those are two different things and where this one currently stands.

```bash
sudo install -m 0755 -d /etc/apt/keyrings
sudo curl -fsSL https://luohoa97.github.io/cordial/apt/cordial-archive-keyring.gpg \
    -o /etc/apt/keyrings/cordial-archive-keyring.gpg
echo "deb [signed-by=/etc/apt/keyrings/cordial-archive-keyring.gpg] https://luohoa97.github.io/cordial/apt stable main" \
    | sudo tee /etc/apt/sources.list.d/cordial.list
sudo apt update
sudo apt install cordial
```

That is the modern, `apt-key`-free form: the key lives in one file named on
the `deb` line, not in a system-wide trusted keyring every other repository
also writes to. `apt update` after that picks up new releases the same way
it does for any other repository; `sudo apt remove cordial` uninstalls, and
your profiles stay in `~/.local/share` until you remove them yourself, same
as every other package format here.

**Verify the key before you trust it.** A `curl` in a doc is exactly the kind
of instruction a supply-chain attack looks like, so check what you just
downloaded against the fingerprint published in
[`docs/design/apt-repository.md`](design/apt-repository.md#the-key), out
of band from this file:

```bash
gpg --show-keys --with-fingerprint /etc/apt/keyrings/cordial-archive-keyring.gpg
```

**Nothing is signed yet.** No `APT_GPG_PRIVATE_KEY` secret exists in this
repository's CI as of this writing, and
[`packaging/apt/build-repo.sh`](../packaging/apt/build-repo.sh) refuses outright
to build an unsigned repository rather than publish one that only works with
`[trusted=yes]` — so the commands above will not install anything until a
maintainer generates and adds the key. This paragraph is here so that gap
does not have to be discovered by `apt update` failing; it is removed the day
signing switches on, in the same commit that adds the fingerprint above.

Verified live on 2026-08-30: `https://luohoa97.github.io/cordial/apt/` and
`.../apt/dists/stable/InRelease` both return 404 while the site root and
`cordial.flatpakrepo` return 200 — exactly what an absent
`APT_GPG_PRIVATE_KEY` predicts, and nothing more, which is worth saying
because a 404 on part of a published site otherwise looks like breakage
rather than an accurately-documented gap.

## dnf (Fedora, RHEL, and derivatives)

**The `.rpm` on the release page installs today and needs no repository.**
Every release attaches one, with a `.cosign.bundle` beside it:

```bash
# From https://github.com/luohoa97/cordial/releases/latest
sudo dnf install ./cordial-*.x86_64.rpm
```

Note the `.fcNN` in the filename: only one Fedora release is built at a time
(Fedora 44 as of this writing), for the reasons in
[`packaging/rpm/build-rpm.sh`](../packaging/rpm/build-rpm.sh)'s header. Verifying
first is worth the two commands — see
[Verifying a release download](#verifying-a-release-download). The repository
below is the nicer route once it is up, and **it is not up yet**: it publishes
nothing until a maintainer adds a signing key, so following these commands
today gets you a 404 rather than Cordial.

Cordial's own repository, not a package in Fedora's own repos — see
[`docs/design/rpm-repository.md`](design/rpm-repository.md) for why
those are two different things and where this one currently stands.

```bash
sudo curl -fsSL https://luohoa97.github.io/cordial/cordial.repo \
    -o /etc/yum.repos.d/cordial.repo
sudo dnf install cordial
```

**Only Fedora 44 has a build today.** The repository is split by
`$releasever` because a `.rpm` built against Fedora 44's `gtk4`/`libadwaita`
is not guaranteed to install on a different release — see
[`docs/design/rpm-repository.md`](design/rpm-repository.md#why-releasever-and-what-that-honestly-costs)
for the full argument. **If your `dnf` reports a `$releasever` other than
44, the command above 404s honestly** rather than installing a build meant
for a different release; that is by design, not a bug to report, until
`release.yml` builds a second release.

**Verify the key before you trust it**, out of band from this file:

```bash
curl -fsSL https://luohoa97.github.io/cordial/rpm/RPM-GPG-KEY-cordial | gpg --show-keys
```

against the fingerprint published in
[`docs/design/rpm-repository.md`](design/rpm-repository.md#the-key).

**Nothing is signed yet.** No `RPM_GPG_PRIVATE_KEY` secret exists in this
repository's CI as of this writing, and
[`packaging/rpm/build-repo.sh`](../packaging/rpm/build-repo.sh) refuses outright
to build an unsigned repository — more strictly than the apt side, because a
dnf `.repo` file with `gpgcheck=0` baked in gives a user nothing to
consciously opt out of the way apt's `[trusted=yes]` does, see
[`docs/design/rpm-repository.md`](design/rpm-repository.md) for why that
asymmetry means this repository is never published unsigned at all. The
commands above will not install anything until a maintainer generates and
adds the key.

## pacman (Arch and derivatives)

**Install the package from the release page. That works today and needs no
key.** Every release attaches a `.pkg.tar.zst` built by the same `makepkg` run
an AUR user's own machine would do, with a `.cosign.bundle` beside it:

```bash
# From https://github.com/luohoa97/cordial/releases/latest
sudo pacman -U cordial-*-x86_64.pkg.tar.zst
```

Verifying it first is two commands and is worth doing — see
[Verifying a release download](#verifying-a-release-download) below. That
signature is keyless, so there is no Cordial key to add to your keyring and
none to trust.

**The AUR is the ordinary route and is blocked, not missing.**
[`packaging/aur/cordial/PKGBUILD`](../packaging/aur/cordial/PKGBUILD) and its
`cordial-git` counterpart are complete and pass `namcap`, but **AUR account
sign-ups are currently closed**, so neither can be submitted. That is the only
thing standing between you and `paru -S cordial`.

**Cordial's own pacman repository is built and publishes nothing.** The
workflow, the `repo-add` script and the `pacman.conf` stanza all exist, and
[`packaging/pacman/build-repo.sh`](../packaging/pacman/build-repo.sh) refuses
outright to build an unsigned repository — so until a maintainer generates a
signing key and adds `ARCH_GPG_PRIVATE_KEY` to CI,
`https://luohoa97.github.io/cordial/arch/` is a 404 and there is nothing to add
to `pacman.conf`. No instructions are printed for it yet, deliberately — a
fingerprint that does not exist and a pacman-key command that cannot work are
worse than saying plainly the repository is not up. See
[`docs/design/pacman-repository.md`](design/pacman-repository.md).

## Verifying a release download

**Every `.deb`, `.rpm`, `.AppImage` and Arch package on a release page is signed**,
and each has a `.cosign.bundle` beside it. The signature is keyless: there is no
Cordial signing key anywhere, and there is nothing for a maintainer to lose. What
the signature proves is that the file came out of this repository's own release
workflow, at that tag, and not from someone who obtained a key.

Install [cosign](https://docs.sigstore.dev/cosign/system_config/installation/), then,
for whichever file you downloaded:

```bash
cosign verify-blob \
  --bundle cordial_0.11.0-1_amd64.deb.cosign.bundle \
  --certificate-identity-regexp '^https://github\.com/luohoa97/cordial/\.github/workflows/release\.yml@refs/tags/v' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  cordial_0.11.0-1_amd64.deb
```

`Verified OK` is the whole of the answer. **Do not drop the two `--certificate-*`
flags** — without them cosign will happily confirm that *somebody* signed the
file, which is not the question you are asking.

The trade is that every signature is recorded permanently in Sigstore's public
transparency log. For public release artefacts that is the point rather than a
cost: it is what lets you check, a year later, that a file was signed by this
workflow at that tag.

This covers the release page. **The Flatpak remote and the APT repository are a
different question and are still unsigned** — those need an OpenPGP key that
Sigstore cannot supply, and the next section says what that means.

## Trust, and what "not signed" means

**The Flatpak remote is not signed.** There is no GPG key on it, so
`flatpak install` verifies that the download matches the repository's own
checksums and nothing beyond that. What it does not do is prove who built it:
anyone who can write to the GitHub Pages site — including anyone who takes
over the GitHub account, and GitHub itself — can serve a different package
under the same name and your machine will install it without complaint. That
is a weaker guarantee than Flathub's and you should know which one you are
getting. Signing is wired up in
[`.github/workflows/flatpak.yml`](../.github/workflows/flatpak.yml) and switches
on the day a maintainer adds a key — the precise procedure for that is written
down in
[`docs/design/flatpak-remote-signing.md`](design/flatpak-remote-signing.md) so
it does not have to be worked out under pressure. A remote added while it was
unsigned stays unverified, so re-add it once signing is live. If you would
rather not extend that trust at all, [building from source](#building-from-source)
below is the whole of the alternative.

**Cordial is not on Flathub, and on current policy it cannot be.** Flathub's
generative-AI policy does not allow applications containing AI-generated or
AI-assisted code, documentation or content, and Cordial contains a great deal of
both — the git history records it in `Co-Authored-By` trailers rather than
hiding it. The policy allows exceptions for mature, well-maintained projects,
and that is the only route; it is not one to take by quietly deleting the
evidence. **This remote is therefore the distribution channel, not a stopgap
until a better one arrives.** Being a third-party client that fetches a
proprietary build at the user's request is not itself the obstacle — Sober's own
published manifest for `org.vinegarhq.Sober` grants `--share=network` and
downloads Roblox's Android build at runtime with no `extra-data` source and
nothing bundled, the same shape this project uses, and it has been live on
Flathub throughout. The AI policy is the whole of what stands in the way, not
what Cordial downloads or when.

> [!NOTE]
> **Measured end to end on 2026-08-05, flatpak 1.18.0**, against the published
> URL rather than a stand-in: `remote-add` accepted, `remote-ls` returning
> `app/io.github.luohoa97.Cordial/x86_64/master`, `install` placing both
> `cordial-shell` and `cordial-run` in `/app/bin`, and `flatpak run` bringing up
> the launcher window and holding it. The appstream branch resolves and the
> metainfo validates, so a software centre lists it too.
>
> **One known limitation of the Flatpak specifically.** The updater asks
> NetworkManager on the system bus whether your connection is metered, the
> sandbox has no system bus, and the check fails closed — so a Flatpak install
> treats every connection as metered and will not download a Roblox build in the
> background unless you turn on *Download on metered connections*. Manual
> downloads are unaffected.
>
> [The workflow](https://github.com/luohoa97/cordial/actions/workflows/flatpak.yml)
> is worth a glance before a fresh install: it publishes only on a green run, so
> a red one on `main` means the remote is serving the previous build.

## Building from source

**You do not need this to run Cordial** — the package routes above are
measured to work. Build from source if you are changing Cordial, if you would
rather not extend trust to an unsigned remote, or if you want a build with your
own patches in it.

Building needs rather more than running does:

- **Clang** — AOSP bionic uses C11 `_Atomic` inside C++ headers and GCC rejects it
- **GTK4 (≥ 4.10) and libadwaita (≥ 1.4)** development packages — the core shell
  in `crates/cordial-shell` is `AdwApplicationWindow`/`AdwToolbarView` end to
  end (see [ADR-002](adr/ADR-002-core-shell-and-ui-handoff.md) and
  [ADR-011](adr/ADR-011-wayland-and-libadwaita.md)), and `gtk4-sys`/
  `libadwaita-sys` link against them via `pkg-config` at build time. Fedora:
  `dnf install gtk4-devel libadwaita-devel`. Debian/Ubuntu:
  `apt install libgtk-4-dev libadwaita-1-dev`. Arch: `pacman -S gtk4 libadwaita`
- **PipeWire's development headers** (`pipewire-devel` / `libpipewire-0.3-dev`),
  optional — for OpenSL ES audio. `native/CMakeLists.txt` detects them via
  `pkg-config` and compiles the real audio backend if found, or the previous
  link-only stub (no sound, but everything else works) if not. Either way
  `libpipewire-0.3.so` itself is `dlopen`'d at run time, never linked, so a
  build made with the headers still runs — audio-less — on a machine that
  only has the runtime library, or neither.

To build the Flatpak yourself, which produces the same package the remote
serves:

```bash
git clone https://github.com/luohoa97/cordial
cd cordial
packaging/build-flatpak.sh --install
```

That one needs no submodules: the manifest pins `third_party/libjnivm` and
`third_party/mcpelauncher-linker` by commit and fetches them itself, and it
pins every crate by the sha256 already in `Cargo.lock`
(`packaging/cargo-sources.json`). flatpak-builder downloads the lot up front;
the compile itself runs with the network unshared, so what comes out is
reproducible ([issue #3](https://github.com/luohoa97/cordial/issues/3)). If you
change a dependency, run `python3 packaging/cargo-sources.py` in the same
commit as the `Cargo.lock` change or the Flatpak build will fail with
`no matching package`.

For development, skip Flatpak and build the binaries directly. This one *does*
want the submodules:

```bash
git clone --recursive https://github.com/luohoa97/cordial
cd cordial
cargo build --release
```
