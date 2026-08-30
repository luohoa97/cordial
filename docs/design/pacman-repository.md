# Cordial's own pacman repository, and why it is not the AUR

Arch has three readings worth keeping separate, more than Debian or Fedora
get in [`apt-repository.md`](apt-repository.md) and
[`rpm-repository.md`](rpm-repository.md), because Arch's own ecosystem has a
third option neither of those has an equivalent of.

1. **Cordial's own pacman repository.** A `repo-add` database at a URL
   Cordial controls, added as a custom repository in `pacman.conf` and
   installed with a plain `pacman -S cordial`, the same shape
   [`packaging/cordial.flatpakrepo`](../../packaging/cordial.flatpakrepo)
   gives `flatpak remote-add`. This is what §1 below covers, and it is
   built -- but, as of this writing, not yet signed or published; see
   "Current status" below.
2. **The AUR.** Arch's own community package repository, where
   `packaging/aur/cordial/PKGBUILD` already lives and already builds --
   `release.yml`'s `arch` job runs `makepkg` against it on every push, and
   that is where the `.pkg.tar.zst` this document's repository packages
   comes from. **Submitting it is currently blocked on AUR account sign-ups
   being closed**, which is a decision Arch's own maintainers made and not
   one this project can route around by building better packaging.
3. **Chaotic-AUR.** A third-party binary repository, already trusted by a
   large share of the Arch community (CachyOS and others enable it by
   default), that does not require an AUR account to submit to. The
   project's submission there is a separate, ongoing piece of work with its
   own repository and its own naming, and this document does not attempt to
   describe it -- see the README's install section for whatever is actually
   live.

**§2 is not a Fedora-shaped "official repository" question the way it is for
Debian and Fedora**, because pacman's own ecosystem does not have one
authority the way apt and dnf each do -- Arch's own repos (`core`, `extra`)
carry a much narrower set of packages than either Debian's or Fedora's, and
almost nothing outside that narrow set is expected to live there. The AUR
and Chaotic-AUR above are the two channels that actually matter for a
package like Cordial, and both are named rather than re-argued from
scratch.

## Current status

**Nobody has generated `ARCH_GPG_PRIVATE_KEY` yet, and this repository's CI
has no such secret as of this writing.** `.github/workflows/pacman.yml`
builds nothing until that changes, for the same reason `yum.yml` and
`apt.yml` do not: its "Import the signing key" step exits cleanly on a
missing secret and every step after it is skipped, so the workflow reports
success while producing no `cordial-pacman-repo` artifact for `flatpak.yml`
to find. `packaging/pacman/build-repo.sh` refuses outright to build an
unsigned repository at all, for the reason given below. Until the secret
exists, `https://luohoa97.github.io/cordial/arch/` 404s.

## §1. Cordial's own repository

### What is signed, and what that does and does not cover

[`packaging/pacman/build-repo.sh`](../../packaging/pacman/build-repo.sh)
takes one or more built `.pkg.tar.zst` files and produces:

```
arch/x86_64/cordial.db.tar.gz          # the repository database, repo-add output
arch/x86_64/cordial.db.tar.gz.sig      # detached signature of the database
arch/x86_64/cordial.files.tar.gz       # file lists, repo-add output
arch/x86_64/cordial.files.tar.gz.sig   # detached signature of the file list
arch/x86_64/cordial-<version>-x86_64.pkg.tar.zst
arch/cordial-archive-keyring.asc       # the public half, for pacman-key --add
```

**The signature is on the database, not on the individual package** -- the
same design [`apt-repository.md`](apt-repository.md) argues for `Release`
and [`rpm-repository.md`](rpm-repository.md) argues for `repomd.xml`,
applied to `repo-add`'s own output. `repo-add -s` signs `cordial.db.tar.gz`
after building it; pacman checks that signature when `SigLevel` requires a
database signature, and refuses a database whose signature does not match.

**Individual `.pkg.tar.zst` are deliberately never signed here.** Unlike
`rpm --addsign`, which rewrites an RPM's own header, `makepkg --sign` would
only add a sidecar `.sig` alongside the existing package file without
touching its bytes -- so, unlike the RPM case, package-level signing here
would not actually conflict with `cosign`'s release-page signature covering
those same bytes. It is left out anyway, for consistency with the other two
channels: this design signs the index once rather than every artefact it
lists, so `pacman.conf`'s `SigLevel` only has to trust the database, never
the package.

### pacman.conf's SigLevel, stated precisely rather than left to a default

Because packages are never individually signed, the `pacman.conf` entry for
this repository has to say so explicitly rather than relying on
`SigLevel`'s repository-wide default, which normally expects both:

```
[cordial]
Server = https://luohoa97.github.io/cordial/arch/$arch
SigLevel = DatabaseRequired PackageNever
```

`DatabaseRequired` demands a valid signature on `cordial.db.tar.gz`, matching
what `repo-add -s` actually produces; `PackageNever` says plainly that
individual packages carry no signature to check, rather than silently
failing every install the way a bare `SigLevel = Required` would once
pacman found no per-package `.sig` to satisfy it. **This combination is
reasoned from `pacman.conf`(5)'s documented `Package`/`Database`-prefixed
keyword syntax, not verified against a running pacman** -- this repository's
own development host has no `pacman` on it (see
`packaging/pacman/build-repo.sh`'s own header for the same constraint
against `repo-add`), so `.github/workflows/pacman.yml`'s own `container:
archlinux:base-devel` job is the first place either gets exercised for
real. If it is wrong, that is where it will show.

### Why this repository refuses to ever publish unsigned

The same argument [`rpm-repository.md`](rpm-repository.md) makes for dnf
applies again here: a `pacman.conf` entry with `SigLevel = Never` baked in
installs exactly as easily as a signed one, with no `[trusted=yes]`-shaped
admission a user has to type themselves first. So
`packaging/pacman/build-repo.sh` refuses outright without
`ARCH_GPG_KEY_ID` set, the same line `packaging/apt/build-repo.sh` and
`packaging/rpm/build-repo.sh` draw, with `--allow-unsigned` reserved for
local inspection and never wired into any CI path that could publish it.

### Generating the key

**Do not reuse the Flatpak, APT or RPM signing keys** -- four separate keys
mean one leaked secret compromises one channel, not four. Same shape, fourth
time:

```bash
# 1. A scratch keyring that disappears when the shell exits.
export GNUPGHOME="$(mktemp -d)"
trap 'rm -rf "$GNUPGHOME"' EXIT

# 2. RSA 4096, sign-only, two-year expiry, no passphrase -- ARCH_GPG_PRIVATE_KEY
#    is imported non-interactively in CI (.github/workflows/pacman.yml's
#    "Import the signing key" step uses `gpg --batch --import`, never
#    `--pinentry-mode loopback --passphrase`).
gpg --batch --passphrase '' --quick-generate-key \
    'Cordial Pacman Repository <choose-an-address-you-monitor>' \
    rsa4096 sign 2y

# 3. Read back the fingerprint -- this becomes ARCH_GPG_KEY_ID.
FPR=$(gpg --list-secret-keys --with-colons | awk -F: '/^fpr/{print $10; exit}')
echo "$FPR"

# 4. Export both halves, armoured. The private half is the exact form
#    .github/workflows/pacman.yml's "Import the signing key" step expects
#    (`printf '%s' "$KEY" | gpg --batch --import`, literal
#    `-----BEGIN PGP PRIVATE KEY BLOCK-----` text). The public half is
#    exported here for the maintainer's own record;
#    packaging/pacman/build-repo.sh re-exports it itself from the imported
#    key during each signed run.
gpg --armor --export-secret-keys "$FPR" > cordial-pacman-signing-key.private.asc
gpg --armor --export             "$FPR" > cordial-pacman-signing-key.public.asc

# 5. Set the two repository secrets .github/workflows/pacman.yml reads.
gh secret set ARCH_GPG_PRIVATE_KEY -R luohoa97/cordial < cordial-pacman-signing-key.private.asc
gh secret set ARCH_GPG_KEY_ID      -R luohoa97/cordial --body "$FPR"

# 6. Rebuild the repository now, rather than waiting for the next push.
gh workflow run pacman.yml -R luohoa97/cordial
```

**GitHub secrets are write-only.** `cordial-pacman-signing-key.private.asc`
needs a durable, access-controlled home before the scratch keyring and loose
`.asc` files are deleted, the same warning `apt-repository.md` and
`rpm-repository.md` give for their own keys.

### What ties this together in CI

1. **`release.yml`**'s existing `arch` job runs `makepkg` against
   `packaging/aur/cordial/PKGBUILD` inside `archlinux:base-devel` and
   uploads the result as the `cordial-arch` artifact, exactly as it did
   before this change.
2. **`.github/workflows/pacman.yml`**, new, triggers when that job's
   workflow finishes, downloads `cordial-arch` from that specific run, and
   runs `build-repo.sh` against it -- but only when `ARCH_GPG_PRIVATE_KEY`
   is set. Unlike `yum.yml` and `apt.yml`, which run on the bare
   `ubuntu-latest` runner, this job's own container is
   `archlinux:base-devel`, because `repo-add` and `pacman-key` are Arch-only
   tools with nothing to `apt-get`.
3. **`flatpak.yml`**'s "Assemble the Pages tree" step looks up
   `pacman.yml`'s latest successful run, downloads `cordial-pacman-repo`
   from it, and copies it into `pages/arch/` alongside `pages/apt/` and
   `pages/rpm/`. Absent a successful run, that step says so and publishes
   without `/arch/`.

The same one-push lag `apt-repository.md` and `rpm-repository.md` each name
applies here too, for the same reason: the assembly step reads the latest
run rather than one from the same commit.

### Installing it, and verifying the key

```bash
curl -fsSL https://luohoa97.github.io/cordial/arch/cordial-archive-keyring.asc \
    | sudo pacman-key --add -
sudo pacman-key --lsign-key <fingerprint>   # from "The key" below
```

Then add the repository to `/etc/pacman.conf` (the block under "pacman.conf's
SigLevel" above), and:

```bash
sudo pacman -Sy cordial
```

`pacman-key --lsign-key` locally signs the imported key, which is what lets
`SigLevel = DatabaseRequired` treat it as trusted rather than merely known --
importing alone is not enough for pacman to accept signatures from it.

### The key

**No key exists yet.** Once `ARCH_GPG_KEY_ID` is set, this section is where
its fingerprint goes, so `pacman-key --lsign-key` above has something to
check against out of band from this file.

### Key rotation

The same failure mode `apt-repository.md` and `rpm-repository.md` describe:
a user who has already run `pacman-key --add`/`--lsign-key` keeps trusting
the old key until they repeat those two commands against the new one, and
`pacman -Sy` starts failing loudly with a signature error in the meantime
rather than silently accepting either key.

### What this does and does not fix

Signing proves `cordial.db.tar.gz` -- and the packages it lists -- was
produced by whoever holds the private key. It does not prove that person is
trustworthy, and it is a different guarantee from the AUR (community
scrutiny of a `PKGBUILD` before anyone runs it) or Arch's own `core`/`extra`
repos (Arch Linux's own trusted-user review). What it fixes is the gap that
exists without it: anyone who can write to the GitHub Pages site can serve a
different package under the same name with no warning to an installed
client, the same gap named for each of the other two channels.
