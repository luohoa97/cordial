# Installing Cordial

<!-- description: Install Cordial from a Flatpak, AppImage, deb, rpm, Arch package, Nix or source, and get Roblox's Android build into it. -->
<!-- icon: download -->

**Use the Flatpak unless you have a reason not to.** It is sandboxed, updates in
place, and runs on one fixed runtime (GNOME 50), so the GTK, WebKit, Vulkan and
audio libraries are the ones Cordial is tested against. Most compatibility
problems reported with the other formats come from those libraries differing.

## What you need

- **x86-64 Linux, or aarch64.** Nothing has been run on real ARM64 hardware;
  [`multiarch.md`](multiarch.md) says what has been checked.
- **A Wayland session.** X11 works too
  ([ADR-024](adr/ADR-024-x11-is-supported-again.md)); Wayland is the primary
  backend.
- **Roblox's official Android client, which you supply.** Cordial ships no
  Roblox code, APK or assets.

### Getting Roblox's build

On first run, press **Download Roblox**. Cordial fetches a build and verifies it
before using it
([ADR-015](adr/ADR-015-fetching-the-roblox-build.md),
[ADR-025](adr/ADR-025-fetching-from-a-third-party-mirror.md)).

- **Sober is installed:** Cordial reads the APK where Sober keeps it,
  `~/.var/app/org.vinegarhq.Sober/data/sober/packages/x86_64/`. Nothing is
  copied or modified, and you never get asked for a build.
- **You have your own APK:** give Settings a path to it, or pass `--apk` on the
  command line. On a split build the engine is in `split_config.x86_64.apk`
  rather than `base.apk`; Cordial checks the sibling files and says which it
  tried if it finds none.

Nothing else is needed to run a package. The Flatpak carries its own toolchain
and libraries.

## Install

Every file on a release page can be verified before you install it; see
[Check what you downloaded](#check-what-you-downloaded).

<!-- tabs -->

### Flatpak

Recommended.

```bash
flatpak remote-add --if-not-exists cordial \
    https://luohoa97.github.io/cordial/cordial.flatpakrepo
flatpak install cordial io.github.luohoa97.Cordial
```

Launch it from your application list, or with
`flatpak run io.github.luohoa97.Cordial`.

| To | Run |
|---|---|
| Update | `flatpak update` |
| Uninstall | `flatpak uninstall io.github.luohoa97.Cordial` |
| Uninstall and delete profiles, sign-in and the extracted Roblox build | `flatpak uninstall --delete-data io.github.luohoa97.Cordial` |

**Branches.** The install above is `stable`, which moves only on a tagged
release. `master` moves on every commit to `main`. The remote once published
only `master`, so an install from before `stable` existed is still on it. To
move it (add `--user` to both if that is how you installed):

```bash
flatpak uninstall io.github.luohoa97.Cordial//master
flatpak install cordial io.github.luohoa97.Cordial//stable
```

To follow `main` on purpose, install `io.github.luohoa97.Cordial//master`.

**Flatpak limitation.** The updater asks NetworkManager on the system bus
whether your connection is metered. The sandbox has no system bus, so the check
fails closed and a Flatpak install treats every connection as metered. It will
not download a Roblox build in the background unless you turn on *Download on
metered connections*. Manual downloads are unaffected.

The [Flatpak workflow](https://github.com/luohoa97/cordial/actions/workflows/flatpak.yml)
publishes only on a green run, so a red run on `main` means the remote is still
serving the previous build.

### AppImage

One file, any distribution, nothing installed system-wide. Download
`Cordial-<version>-<commit>-x86_64.AppImage` (or `-aarch64`) from the
[releases page](https://github.com/luohoa97/cordial/releases):

```bash
chmod +x Cordial-*.AppImage
./Cordial-*.AppImage
```

It carries GTK4, libadwaita and WebKitGTK. Delete the file to remove it;
profiles stay in `~/.local/share` until you delete them. It needs FUSE; if it
refuses to start, run it with `--appimage-extract-and-run` to unpack to a
temporary directory instead. Updates are manual.

**The web view is not established on a real machine.** The sign-in window uses
WebKitGTK, which finds its helper processes through paths fixed when it was
built, and the AppImage works around that inside a private mount namespace. That
needs `bwrap` and unprivileged overlay mounts; if your kernel refuses either, the
AppImage says so on standard error and the web view then needs WebKitGTK 6.0
installed at Fedora's path. This has been measured on a stand-in for a machine
with no WebKitGTK, and only for Fedora. If the sign-in window is blank, report it
with whatever the terminal printed. The Flatpak is unaffected. Background:
[ADR-032](adr/ADR-032-appimage-build-base-moves-to-ubuntu-24-04.md).

### Debian / Ubuntu

The `.deb` on the [release page](https://github.com/luohoa97/cordial/releases/latest)
installs with no repository:

```bash
sudo apt install ./cordial_*_amd64.deb     # or _arm64.deb
```

Cordial's own apt repository keeps you current through `apt upgrade`. It is
Cordial's repository, not a package in Debian or Ubuntu
([design note](design/apt-repository.md)):

```bash
sudo install -m 0755 -d /etc/apt/keyrings
sudo curl -fsSL https://luohoa97.github.io/cordial/apt/cordial-archive-keyring.gpg \
    -o /etc/apt/keyrings/cordial-archive-keyring.gpg
echo "deb [signed-by=/etc/apt/keyrings/cordial-archive-keyring.gpg] https://luohoa97.github.io/cordial/apt stable main" \
    | sudo tee /etc/apt/sources.list.d/cordial.list
sudo apt update
sudo apt install cordial
```

`sudo apt remove cordial` uninstalls; profiles stay in `~/.local/share`. Check
the key against its fingerprint before trusting it, see
[Check what you downloaded](#check-what-you-downloaded).

### Fedora / RHEL

The `.rpm` on the [release page](https://github.com/luohoa97/cordial/releases/latest)
installs with no repository:

```bash
sudo dnf install ./cordial-*.x86_64.rpm    # or .aarch64.rpm
```

**Only Fedora 44 has a build.** The `.fcNN` in the filename is the Fedora release
it was built against, and only one is built at a time. The repository is split by
`$releasever`, so on any other release the command below returns a 404 rather
than installing a build meant for a different Fedora. That is by design, not a
bug to report ([design note](design/rpm-repository.md)).

Cordial's own dnf repository, on Fedora 44:

```bash
sudo curl -fsSL https://luohoa97.github.io/cordial/cordial.repo \
    -o /etc/yum.repos.d/cordial.repo
sudo dnf install cordial
```

Check the key first, see [Check what you downloaded](#check-what-you-downloaded).

### Arch

Install the package from the [release page](https://github.com/luohoa97/cordial/releases/latest).
It is built by the same `makepkg` run an AUR user would do, and needs no key:

```bash
sudo pacman -U cordial-*-x86_64.pkg.tar.zst
```

Arch is x86-64 only; Arch Linux does not build for aarch64.

**The AUR packages are not published by this project.** `cordial`, `cordial-bin`
and `cordial-git` on the AUR belong to an account that is not the maintainer's
(`taxin-404`). On 2026-09-30 they were copies of
[`packaging/aur/`](../packaging/aur) from 0.17.0, fetching only from this
repository. Nobody here reviews what that account publishes next, so prefer the
release package or Cordial's own repository.

Cordial's own pacman repository. Import its key, then add the stanza to
`pacman.conf`:

```bash
curl -fsSL https://luohoa97.github.io/cordial/arch/cordial-archive-keyring.asc \
    | sudo pacman-key --add -
sudo pacman-key --lsign-key C82BBD7D82744F804A68DA8B3A69D3241BA6288F
```

```ini
[cordial]
Server = https://luohoa97.github.io/cordial/arch/$arch
SigLevel = DatabaseRequired PackageNever
```

The database is signed and the packages are not, which is what
`DatabaseRequired PackageNever` says
([design note](design/pacman-repository.md)). **INFERRED:** `pacman -Sy` and
this `pacman.conf` stanza have not been run; the `SigLevel` line comes from
`pacman.conf(5)`.

### Nix

The flake builds a package for x86-64 Linux. From a checkout:

```bash
nix build '.?submodules=1#cordial'
```

The submodules need `?submodules=1`; flakes do not fetch them otherwise. **It has
never been launched into a game.** What has been run: the package builds, and
`cordial --help` and `cordial --diagnostics` run from the output.
`cordial --diagnostics` reports `Install unknown`, because nothing recognises a
`/nix/store` path yet. The remote form,
`nix run "github:luohoa97/cordial?submodules=1"`, is **INFERRED** from the local
build and has not been tried. More in
[CONTRIBUTING.md](../CONTRIBUTING.md#or-use-the-flake).

### From source

You do not need this to run Cordial. Build from source if you are changing it,
want your own patches in, or would rather not extend trust to a remote signed by a
key held as a CI secret.

```bash
git clone --recursive https://github.com/luohoa97/cordial
cd cordial
cargo build --release
```

You need:

- **Clang.** AOSP bionic uses C11 `_Atomic` inside C++ headers and GCC rejects it.
- **GTK4 (4.12 or newer) and libadwaita (1.5 or newer) development packages.**
  Fedora: `dnf install gtk4-devel libadwaita-devel`. Debian/Ubuntu:
  `apt install libgtk-4-dev libadwaita-1-dev`. Arch: `pacman -S gtk4 libadwaita`.
- **Boost's headers**, only for Play in VR, which a plain `cargo build` leaves
  out: build with `--features cordial-runtime/vr` and install `boost-devel`,
  `libboost-dev` or `boost`. It is for dynarmic, the VR mode's translator, which
  `--recursive` fetches. Every package has VR turned on.
- **PipeWire's development headers** (`pipewire-devel`, `libpipewire-0.3-dev`),
  optional. With them the build includes the OpenSL ES audio backend; without
  them there is no sound and everything else works. `libpipewire-0.3.so` is
  loaded at run time, never linked, so a build made with the headers still runs
  on a machine that only has the runtime library, or neither.

To build the Flatpak yourself, which produces what the remote serves, no
submodules needed:

```bash
git clone https://github.com/luohoa97/cordial
cd cordial
packaging/build-flatpak.sh --install
```

If you change a dependency, run `python3 packaging/cargo-sources.py` in the same
commit as the `Cargo.lock` change, or the Flatpak build fails with
`no matching package`
([issue #3](https://github.com/luohoa97/cordial/issues/3)).

<!-- /tabs -->

## Check what you downloaded

Files on a release page and the package repositories are verified differently.

<details>
<summary>Release-page files (cosign)</summary>

Every `.deb`, `.rpm`, `.AppImage` and Arch package on a release page has a
`.cosign.bundle` beside it. The signature is keyless: there is no Cordial signing
key to trust or lose. It proves the file came out of this repository's own
release workflow at that tag. Install
[cosign](https://docs.sigstore.dev/cosign/system_config/installation/), then:

```bash
cosign verify-blob \
  --bundle cordial_0.11.0-1_amd64.deb.cosign.bundle \
  --certificate-identity-regexp '^https://github\.com/luohoa97/cordial/\.github/workflows/release\.yml@refs/tags/v' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  cordial_0.11.0-1_amd64.deb
```

`Verified OK` is the whole answer. **Do not drop the two `--certificate-*`
flags.** Without them cosign confirms that somebody signed the file, which is not
the question you are asking. Every signature is recorded in Sigstore's public
transparency log.

This covers the release page only. The Flatpak remote and the apt, dnf and
pacman repositories use OpenPGP keys, below.

</details>

<details>
<summary>Flatpak remote key</summary>

The published `cordial.flatpakrepo` carries a GPG key, and the repository summary
has a detached signature, so `flatpak install` checks the download was signed by
it. Fingerprint:

```text
8364 5E9B 8F6C 4B29 227D  4629 4310 E617 967A BDD8
```

**A remote added while it was unsigned stays unverified.** Flatpak recorded
`gpg-verify=false` then, and a later signed definition does not change it. If you
added the remote before the key existed, remove and re-add it
(`flatpak remote-delete cordial`, then the `remote-add` above).

**INFERRED:** the fingerprint was read from the published file on 2026-09-30, not
confirmed out of band. That day `summary.sig` was published and `ostree remote
refs` verified the summary against the key in a throwaway repository;
`flatpak install` was not run against it.

A signature does not make GitHub Pages trustworthy: whoever holds the private
key, a repository secret, can sign anything. That is a weaker arrangement than
Flathub's, and if you would rather not extend that trust, build from source.
Cordial is not on Flathub and, on its current generative-AI policy, cannot be; the
remote is the distribution channel, not a stopgap
([why](HANDOVER.md#flathub-and-why-it-is-not-the-plan),
[signing procedure](design/flatpak-remote-signing.md)).

</details>

<details>
<summary>apt repository key</summary>

```bash
gpg --show-keys --with-fingerprint /etc/apt/keyrings/cordial-archive-keyring.gpg
```

```text
E6BE 3043 5BD6 3471 FD1A  B331 DC05 1D16 7161 8AA6
```

`dists/stable/InRelease` carries an OpenPGP signature from this key. **INFERRED:**
the fingerprint was read from the published keyring on 2026-09-30, not confirmed
out of band. `InRelease` verified against that keyring; `apt install` was not
run.

</details>

<details>
<summary>dnf repository key</summary>

```bash
curl -fsSL https://luohoa97.github.io/cordial/rpm/RPM-GPG-KEY-cordial | gpg --show-keys
```

```text
E5FA CC1B D170 8EC9 4FFF  5817 FDD1 0A8B 6D7F 10B9
```

The `.repo` file sets `repo_gpgcheck=1` and `gpgcheck=0`: each release
directory's `repodata/repomd.xml` is signed, not the individual `.rpm`
([design note](design/rpm-repository.md)). **INFERRED:** the fingerprint was read
from the published site on 2026-09-30, not confirmed out of band.
`rpm/44/x86_64/repodata/repomd.xml.asc` verified against it; `dnf install` was
not run.

</details>

<details>
<summary>pacman repository key</summary>

Fingerprint, as used in the `pacman-key --lsign-key` command above:

```text
C82BBD7D82744F804A68DA8B3A69D3241BA6288F
```

**INFERRED:** read from the published keyring on 2026-09-30, not confirmed out of
band. `cordial.db.sig` verified against it. The release-page package needs no key
at all, see the cosign section.

</details>
