# Cordial's own dnf/yum repository, and why it is not Fedora itself

"Publish Cordial to Fedora's repositories" has two readings, the same shape
[`apt-repository.md`](apt-repository.md) draws for Debian and Ubuntu: one
ships this week, and the other is a review process Cordial does not control
the outcome of.

1. **Cordial's own dnf repository.** An `rpm/<releasever>/<arch>/` tree at a
   URL Cordial controls, that a user drops a `.repo` file for and then runs
   `dnf install cordial` against, the same shape
   [`packaging/cordial.flatpakrepo`](../../packaging/cordial.flatpakrepo)
   already gives `flatpak remote-add` and
   [`apt-repository.md`](apt-repository.md) gives `apt`. This is what §1
   below covers, and it is built -- but, as of this writing, not yet signed
   or published; see "Current status" below before assuming the commands in
   this document already work.
2. **The `cordial` package in Fedora proper** -- in the repos every `dnf
   install` reaches by default, with no third-party `.repo` file added
   first. §2 below is the honest assessment of that path, and it is not
   started.

## Current status

**Nobody has generated `RPM_GPG_PRIVATE_KEY` yet, and this repository's CI has
no such secret as of this writing.** `.github/workflows/yum.yml` builds
nothing until that changes -- its "Import the signing key" step exits
cleanly on a missing secret and every step after it is skipped, so the
workflow reports success while producing no `cordial-rpm-repo` artifact for
`flatpak.yml` to find. `packaging/rpm/build-repo.sh` itself refuses outright
to build an unsigned repository at all, for the reason given below, so there
is no lesser artifact it could produce instead. Until the secret exists,
`https://luohoa97.github.io/cordial/rpm/` and every URL under it 404 --
cleanly, the same way `https://luohoa97.github.io/cordial/apt/` does today
for the identical reason on the apt side (verified directly: both `/apt/`
and `/apt/dists/stable/InRelease` return 404 against the live site while the
site root and `cordial.flatpakrepo` both return 200, and the `apt.yml` run
log names the missing secret as the reason in those exact words). This
section is here so that gap is discovered by reading this document rather
than by `dnf install` failing; it is removed the day signing switches on, in
the same commit that adds the fingerprint under "The key" below.

## §1. Cordial's own repository

### Why $releasever, and what that honestly costs

**The published tree is split by Fedora release, not one flat directory.**
`packaging/rpm/cordial.spec` requires `%global toolchain clang` and Fedora
44's `gtk4` (4.22) and `libadwaita` (1.9) -- Fedora 43 ships older versions of
both and fails to build against, which is why `release.yml`'s `rpm` job runs
inside `registry.fedoraproject.org/fedora:44` specifically (see
`packaging/rpm/build-rpm.sh`'s own header). A `.rpm` built against Fedora 44's
libraries is not guaranteed installable on Fedora 43 or 45, so serving one
binary from one flat directory to every release's dnf would either silently
work by luck or silently fail by an unrelated missing symbol -- neither of
which points a user at the actual cause.

So `packaging/cordial.repo`'s `baseurl` carries dnf's own `$releasever` and
`$basearch` variables, and `packaging/rpm/build-repo.sh` publishes each
`.rpm` it is given into `rpm/<releasever>/<arch>/`, reading the release
number out of the filename's own `.fcNN.` tag (`make-srpm.sh`'s
`%dist` -- e.g. `cordial-0.12.1-1.fc44.x86_64.rpm`, the actual filename on
the v0.12.1 release page) rather than guessing it.

**As of this writing, that means exactly one directory exists: `rpm/44/`.**
`release.yml` builds only Fedora 44 today, so a user running Fedora 43, 45,
or a Fedora-derivative reporting a different `$releasever` gets a plain 404
from `dnf install`, not a wrong package silently installed and not a
confusing dependency error three layers down. **This is the correct failure
and it is not a gap to paper over with a "latest" symlink or a merged
directory** -- a 404 tells a user precisely what happened (nothing is built
for their release yet) where a wrong-release `.rpm` would tell them nothing
until it broke. Widening past Fedora 44 is a `release.yml` matrix change,
not a change to this document's design, and the day it happens this
paragraph should say so rather than continuing to describe one release.

### What is signed, and what that does and does not cover

[`packaging/rpm/build-repo.sh`](../../packaging/rpm/build-repo.sh) takes one
or more built `.rpm` files and produces, per release directory:

```
rpm/<releasever>/<arch>/repodata/repomd.xml       # metadata manifest
rpm/<releasever>/<arch>/repodata/repomd.xml.asc   # detached signature of repomd.xml
rpm/<releasever>/<arch>/repodata/primary.xml.gz   # package index, createrepo_c output
rpm/<releasever>/<arch>/repodata/filelists.xml.gz # file lists, createrepo_c output
rpm/<releasever>/<arch>/repodata/other.xml.gz     # changelogs, createrepo_c output
rpm/<releasever>/<arch>/cordial-<version>.<arch>.rpm
rpm/RPM-GPG-KEY-cordial                            # the public half, for a user's .repo
```

**The signature is on `repomd.xml`, not on the individual `.rpm`** -- the same
design [`apt-repository.md`](apt-repository.md) argues for signing `Release`
rather than the `.deb`, applied to dnf's own metadata shape. `repomd.xml`'s
manifest carries the checksum of `primary.xml.gz`; `primary.xml.gz` carries
the checksum of every `.rpm` `createrepo_c` indexed. One signature roots that
whole hash chain, so tampering with the `.rpm` changes a hash two files up
from the actual signature and `dnf` refuses the install just the same as if
`repomd.xml` itself had been altered.

**Individual `.rpm` are deliberately never re-signed.** `rpm --addsign`
rewrites the package's own header in place, which would put a different file
at this URL than the one `release.yml` built and its own `cosign` step
already signed on the release page -- the byte-identity concern
[`flatpak.yml`](../../.github/workflows/flatpak.yml)'s own comment on the
single-file bundle raises for exactly the same reason ("Built from the same
`repo` the remote is published from... not a second build that might
differ"). So `packaging/cordial.repo` carries `gpgcheck=0` **permanently**,
not as a placeholder waiting for a future `1` -- the setting that actually
carries this repository's trust guarantee is `repo_gpgcheck=1`, checked
against `repomd.xml.asc`, not `gpgcheck`.

### Why this repository refuses to ever publish unsigned, and apt's own script is the reason the argument had to be made explicit

[`apt-repository.md`](apt-repository.md) explains why
`packaging/apt/build-repo.sh` refuses to build an unsigned apt tree: an
unsigned repository still produces every file `apt` needs, and the only
thing standing between a user and installing from it is typing
`[trusted=yes]` into their own `sources.list` -- a visible, deliberate
downgrade a user has to type themselves.

**dnf has no equivalent friction, and that makes the unsigned case worse, not
milder.** A `.repo` file that ships with `gpgcheck=0` baked in installs
exactly as easily as one with `gpgcheck=1` -- there is no keyword a user
must add themselves to accept the weaker guarantee, because the file already
made that choice for them before they ever saw it. An attacker who can write
to the Pages site can serve a different `.rpm` under the same name with
nobody having had to opt into trusting an unverified source in the first
place. So `packaging/rpm/build-repo.sh` draws the same line
`packaging/apt/build-repo.sh` does, for a stronger reason than the one that
produced it there: **no `RPM_GPG_KEY_ID`, no repository**, full stop, with
`--allow-unsigned` reserved for inspecting the tree's shape locally and never
wired into any CI path that could publish it.

### Generating the key

**Do not reuse the Flatpak or APT signing keys.** Three separate keys mean a
single leaked secret compromises one channel rather than three, for the cost
of running `--quick-generate-key` twice more. The procedure is the same
shape as the other two -- a scratch keyring, no passphrase (CI has no
terminal to answer a prompt on), `sign`-only usage, an expiry chosen on
purpose -- run as a numbered sequence a maintainer can follow without
already knowing GPG:

```bash
# 1. A scratch keyring that disappears when the shell exits, so the key
#    never touches this machine's own GNUPGHOME.
export GNUPGHOME="$(mktemp -d)"
trap 'rm -rf "$GNUPGHOME"' EXIT

# 2. Generate the key: RSA 4096, sign-only capability, two-year expiry, no
#    passphrase (RPM_GPG_PRIVATE_KEY is imported non-interactively in CI --
#    see .github/workflows/yum.yml's "Import the signing key" step, which
#    uses `gpg --batch --import` and never `--pinentry-mode loopback
#    --passphrase`, so the key must need no passphrase to sign with).
gpg --batch --passphrase '' --quick-generate-key \
    'Cordial RPM Repository <choose-an-address-you-monitor>' \
    rsa4096 sign 2y

# 3. Read back the fingerprint -- this is what RPM_GPG_KEY_ID becomes.
FPR=$(gpg --list-secret-keys --with-colons | awk -F: '/^fpr/{print $10; exit}')
echo "$FPR"

# 4. Export both halves. The private half is armoured plain text (the exact
#    form .github/workflows/yum.yml's "Import the signing key" step expects
#    -- it does `printf '%s' "$KEY" | gpg --batch --import`, which wants the
#    literal `-----BEGIN PGP PRIVATE KEY BLOCK-----` text, not base64 and not
#    a binary export). The public half is exported the same way here for
#    convenience; packaging/rpm/build-repo.sh re-exports it itself from the
#    imported key during each signed run, so this copy is for the maintainer
#    generating the key to keep, not something CI reads.
gpg --armor --export-secret-keys "$FPR" > cordial-rpm-signing-key.private.asc
gpg --armor --export             "$FPR" > cordial-rpm-signing-key.public.asc

# 5. Set the two repository secrets .github/workflows/yum.yml reads --
#    KEY_ID is the bare 40-character fingerprint from step 3, not the short
#    form (a collision-attack target, and every --local-user/--export call
#    in build-repo.sh and yum.yml takes either, so there is no reason to use
#    the weaker one).
gh secret set RPM_GPG_PRIVATE_KEY -R luohoa97/cordial < cordial-rpm-signing-key.private.asc
gh secret set RPM_GPG_KEY_ID      -R luohoa97/cordial --body "$FPR"

# 6. Rebuild the repository now, rather than waiting for the next push --
#    .github/workflows/yum.yml's own workflow_dispatch trigger exists for
#    exactly this moment.
gh workflow run yum.yml -R luohoa97/cordial
```

**GitHub secrets are write-only.** There is no "view" button once saved, so
`cordial-rpm-signing-key.private.asc` needs a durable, access-controlled home
-- a password manager's file storage, an encrypted volume -- before the
scratch keyring and loose `.asc` files are deleted. Losing the only copy
means the next key generated cannot re-sign anything the old one did, and
every user who fetched the old `RPM-GPG-KEY-cordial` needs telling, which is
the rotation case below.

### What ties this together in CI

Three files, three jobs, in order, the same shape
[`apt-repository.md`](apt-repository.md) describes for apt:

1. **`release.yml`**'s existing `rpm` job builds
   `cordial-<version>-<release>.fc44.x86_64.rpm` inside
   `registry.fedoraproject.org/fedora:44` and uploads it as the `cordial-rpm`
   artifact, exactly as it did before this change.
2. **`.github/workflows/yum.yml`**, new, triggers when that job's workflow
   finishes, downloads `cordial-rpm` from that specific run, and runs
   `build-repo.sh` against it -- but only when `RPM_GPG_PRIVATE_KEY` is set;
   otherwise it stops after saying so, and produces no artifact at all
   rather than an unsigned one (see "Why this repository refuses to ever
   publish unsigned" above -- this is the one place this design diverges
   from a softer "publish it anyway, unsigned" reading, and it diverges on
   purpose).
3. **`flatpak.yml`** is the one workflow that deploys to GitHub Pages, for
   the reason its own long header gives and `apt-repository.md` repeats:
   Pages has no per-path incremental publish, so a second workflow deploying
   independently would replace the site rather than add to it.
   `flatpak.yml`'s "Assemble the Pages tree" step looks up `yum.yml`'s latest
   successful run, downloads `cordial-rpm-repo` from it, and copies it into
   `pages/rpm/` alongside the existing `pages/apt/` and `pages/repo/`. If no
   successful `yum.yml` run exists yet -- no key, or it has simply never run
   -- that step says so and publishes without `/rpm/`, the graceful
   degradation `pages/apt/` already uses.

**The same one-push lag applies here that `apt-repository.md` names for
`/apt/`:** because the assembly step reads `yum.yml`'s *latest* run rather
than one from the same commit, the dnf tree in a given Pages deploy can lag
the flatpak repo beside it by up to one push. Fixing it means merging the
publishing workflows into one run, which has not been attempted for the same
reason `apt-repository.md` gives: it is a much larger change to a file that
has already cost real debugging time to get right.

### Installing it, and verifying the key

```bash
sudo curl -fsSL https://luohoa97.github.io/cordial/cordial.repo \
    -o /etc/yum.repos.d/cordial.repo
sudo dnf install cordial
```

**Verify the key before you trust it**, out of band from this file:

```bash
curl -fsSL https://luohoa97.github.io/cordial/rpm/RPM-GPG-KEY-cordial | gpg --show-keys
```

### The key

**No key exists yet.** Once `RPM_GPG_KEY_ID` is set, this section is where
its fingerprint goes, published out of band from the install commands above
so a user has a second source to check against. Until this section names a
fingerprint, there is nothing at the published URL to check it against in
the first place.

### Key rotation

**A user who already has `/etc/yum.repos.d/cordial.repo` keeps trusting the
old key until they replace it, the same correct failure mode
`apt-repository.md` describes for apt.** dnf checks the current
`repomd.xml.asc` against whatever `gpgkey=` in the `.repo` file points at, on
every `dnf install`/`dnf update`. If the signing key is ever rotated, new
`repomd.xml` files signed by the new key will not verify against a
`RPM-GPG-KEY-cordial` a user's dnf has already imported and cached, and dnf
starts failing loudly with a signature error rather than silently accepting
either key. The fix is the same shape: re-fetch the key at the published URL
(`sudo dnf clean all` if dnf has cached the old one under
`/etc/pki/rpm-gpg/` or its own cache).

### What this does and does not fix

Signing proves `repomd.xml` -- and everything chained under it -- was
produced by whoever holds the private key. It does not prove that person is
trustworthy, and it is a different guarantee from Fedora's own repos, which
additionally review what a package *does*. What it fixes is the gap that
exists without it: anyone who can write to the GitHub Pages site can serve a
different `.rpm` under the same name with no warning to an installed
client, the same gap [`flatpak-remote-signing.md`](flatpak-remote-signing.md)
and [`apt-repository.md`](apt-repository.md) each name for their own
channel.

## §2. Official Fedora: an honest assessment

**Not started, and the case for starting it is weaker than it looks** -- the
same honest framing `apt-repository.md` gives Debian, applied to Fedora's
own process, which is stricter in the one place that matters most for
Cordial.

**The process itself.** Fedora does not accept a package directly from an
upstream project either. It needs:

- A Fedora account and a package review request filed in Fedora's Pagure
  (`fedora-review`), the rough equivalent of Debian's ITP bug.
- A spec that satisfies the Fedora Packaging Guidelines -- closer to
  `packaging/rpm/cordial.spec` in shape than Debian's packaging would be to
  `packaging/deb/`, since both are native RPM/spec formats, but Fedora's
  guidelines are their own document and a formal review checks the spec line
  by line against them, not against whatever already builds.
- A sponsor: an existing Fedora packager who reviews and can vouch for a new
  contributor before they can commit packages themselves, the same
  gatekeeping role Debian's sponsor plays. Cordial has none today.

**Licensing is not the obstacle**, for the same reason `apt-repository.md`
gives: Cordial itself is GPL-3.0-or-later, and the vendored third-party code
in [`NOTICE`](../../NOTICE) is independently permissively licensed
throughout.

**What would: Fedora's licensing and content rules are stricter than
Debian's `main`/`contrib` split, not looser.** Debian at least has a
`contrib` section for software that is itself free but depends on
non-free software to be useful -- the section `apt-repository.md` argues
Cordial's own shape (loading a proprietary binary it fetches at runtime)
would realistically land in, if anywhere. **Fedora has no equivalent
section in its own official repositories.** Software whose purpose depends
on something Fedora will not ship itself does not have a softer landing
inside Fedora proper the way it does in Debian; it simply is not accepted
there, on Fedora's own stated packaging guidelines regarding software that
requires non-free content to function usefully.

**RPM Fusion is the closest real parallel to Debian's `contrib`, and it is
not Fedora.** RPM Fusion is a long-running, well-known third-party
repository run by a separate community specifically to host packages Fedora
itself will not carry for licensing or patent reasons -- multimedia codecs,
proprietary graphics drivers, and games or launchers built around one
specific non-free target are its ordinary content, which is exactly
Cordial's shape. Submitting to RPM Fusion is a real option this document
does not rule out, but it is **a separate community with its own review and
its own account system**, not a shortcut into Fedora proper, and nothing
towards it has been started either. Whether it is worth pursuing instead of,
or alongside, this document's §1 is a decision for whoever holds the project
to make, not one this document resolves on Cordial's behalf.

**The honest conclusion:** a Fedora review request is realistic to *file* at
any point, but landing in Fedora's own repositories is not the likely
outcome given the dependency this project has on a runtime-fetched
proprietary binary, and RPM Fusion, if pursued, is a different project's
process with its own timeline. **§1 is therefore the plan, not a stopgap for
§2** -- the same framing `apt-repository.md` gives Debian and
[`flatpak-remote-signing.md`](flatpak-remote-signing.md) gives Flathub, for
the same underlying reason: the channel Cordial controls is the one that can
actually ship this week.
