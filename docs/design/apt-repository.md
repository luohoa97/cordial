# Cordial's APT repository, and why it is not Debian itself

"Publish Cordial to a Debian repository" has two readings, and they are not
close cousins -- one ships this week, and the other is a multi-month process
Cordial does not control the outcome of. This document is both halves,
written out separately on purpose, because conflating them is the way this
kind of request goes sideways.

1. **Cordial's own APT repository.** A `dists/`+`pool/` tree at a URL Cordial
   controls, that a user adds to `sources.list` and then runs `apt install
   cordial` against, the same shape [`packaging/cordial.flatpakrepo`](../../packaging/cordial.flatpakrepo)
   already gives `flatpak remote-add`. This is what §1 below covers, and it
   is built.
2. **The `cordial` package in Debian proper** -- in the archive every
   `apt install` reaches by default, with no third-party line added first.
   That needs a Debian Developer to sponsor the upload, an ITP (Intent To
   Package) bug, packaging that satisfies Debian Policy, and months rather
   than days. §2 below is the honest assessment of that path and stops
   there; nothing towards it has been started.

## §1. Cordial's own repository

### What is signed, and what that does and does not cover

[`packaging/apt/build-repo.sh`](../../packaging/apt/build-repo.sh) takes one
or more built `.deb` files and produces:

```
dists/stable/Release            # suite metadata + a SHA256/MD5Sum manifest
dists/stable/Release.gpg        # detached signature of Release, for apt < 1.1
dists/stable/InRelease           # Release, inline-clearsigned, for apt >= 1.1
dists/stable/main/binary-amd64/Packages(.gz)
pool/main/c/cordial/cordial_<version>_amd64.deb
cordial-archive-keyring.gpg     # the public half, for a user's sources.list
```

**The signature is on `Release`, not on the individual `.deb`.** That is
not a shortcut -- it is how every apt repository works, Debian's own
included. `Release`'s manifest carries the SHA256 of `Packages`; `Packages`
carries the SHA256 of the `.deb`. One signature roots a hash chain that
covers everything under it, so tampering with the `.deb` changes a hash two
files up the chain from the actual signature and `apt` refuses the install
just the same as if `Release` itself had been altered. There is no separate
per-package signature (`dpkg-sig`, `debsig-verify`) anywhere in this
pipeline, and none is needed for that reason.

**No key, no repository.** Unlike [flatpak-remote-signing.md](flatpak-remote-signing.md)'s
OSTree remote, which is a meaningful (if weaker) thing to publish unsigned
because OSTree's own object checksums give a baseline nobody has to opt out
of, an apt repository with no signature only works at all if a user adds
`[trusted=yes]` to their `sources.list` line -- and that is a worse thing to
hand someone than an error. So `build-repo.sh` refuses outright when
`APT_GPG_KEY_ID` is unset, rather than writing a tree that has every file an
installable repository needs except the one that makes it trustworthy. See
that script's own header for the refusal message a maintainer or CI sees.
`--allow-unsigned` overrides it for inspecting the tree's shape locally;
publishing what it produces is the one thing that flag does not make
acceptable, and it is not wired into any CI path that could publish it.

### Generating the key

**Status: nobody has generated one, and this repository's CI has no
`APT_GPG_PRIVATE_KEY` secret as of this writing.** The commands below build
nothing until that changes, and both the README and
[`.github/workflows/apt.yml`](../../.github/workflows/apt.yml) say so
plainly rather than implying a working install path that is not there yet.

**Do not reuse the Flatpak signing key.** Both are plain OpenPGP signatures
and gpg has no objection to signing two unrelated things with one key, but
[flatpak-remote-signing.md](flatpak-remote-signing.md)'s reasoning for a
dedicated key applies again here, harder: one key covering two distribution
channels means one leaked secret compromises both at once, for the cost of
running `--quick-generate-key` a second time. Generate a second, separate
key.

The procedure is the same shape as the Flatpak one -- a scratch keyring, no
passphrase (CI has no terminal to answer a prompt on), `sign`-only usage,
an expiry chosen on purpose:

```bash
export GNUPGHOME="$(mktemp -d)"
trap 'rm -rf "$GNUPGHOME"' EXIT

gpg --batch --passphrase '' --quick-generate-key \
    'Cordial APT Repository <choose-an-address-you-monitor>' \
    rsa4096 sign 2y

FPR=$(gpg --list-secret-keys --with-colons | awk -F: '/^fpr/{print $10; exit}')
echo "$FPR"

gpg --armor --export-secret-keys "$FPR" > cordial-apt-signing-key.private.asc
gpg --armor --export             "$FPR" > cordial-apt-signing-key.public.asc
```

### Where the private half lives

**A GitHub Actions repository secret, and nowhere else.** Either the web UI
(Repository → Settings → Secrets and variables → Actions → New repository
secret) or the `gh` CLI, which is the faster path from the same shell the key
was just generated in and reads exactly the same bytes
`.github/workflows/apt.yml`'s "Import the signing key" step later imports:

```bash
gh secret set APT_GPG_PRIVATE_KEY -R luohoa97/cordial < cordial-apt-signing-key.private.asc
gh secret set APT_GPG_KEY_ID      -R luohoa97/cordial --body "$FPR"

# Rebuild now, rather than waiting for the next push --
# .github/workflows/apt.yml's own workflow_dispatch trigger exists for
# exactly this moment.
gh workflow run apt.yml -R luohoa97/cordial
```

Two secrets, named precisely because `apt.yml` reads them by these names and
nothing else:

- `APT_GPG_PRIVATE_KEY` -- the full contents of
  `cordial-apt-signing-key.private.asc`, `-----BEGIN...-----` lines
  included. **Armoured plain text, not base64** -- confirmed by reading
  `apt.yml`'s own "Import the signing key" step, which does
  `printf '%s' "$KEY" | gpg --batch --import` and never decodes anything
  first, so the secret must already be the literal ASCII-armoured block.
- `APT_GPG_KEY_ID` -- the bare 40-character fingerprint, `$FPR` above. The
  full fingerprint, not the short form -- the short form is what a
  collision attack targets, and every `--local-user`/`--export` call in
  `build-repo.sh` and `apt.yml` takes either.

[`.github/workflows/apt.yml`](../../.github/workflows/apt.yml) imports it
into a scratch `GNUPGHOME` the same way `flatpak.yml` already does for its
own key -- `gpg --batch --import`, never `--pinentry-mode loopback
--passphrase`, which is why the key must need no passphrase to sign with in
the first place. **GitHub secrets are write-only.** There is no "view"
button once saved, so `cordial-apt-signing-key.private.asc` needs a durable,
access-controlled home -- a password manager's file storage, an encrypted
volume -- before the scratch keyring and loose `.asc` files are deleted.
Losing the only copy means the next key generated cannot re-sign anything
the old one did, and every user who fetched the old
`cordial-archive-keyring.gpg` needs telling, which is the rotation case
below.

### What ties this together in CI, and the one thing that follows from it

Three files, three jobs, in order:

1. **`release.yml`**'s existing `deb` job builds `cordial_<version>_amd64.deb`
   inside `ubuntu:25.10` (the container this workspace has been shown to
   satisfy `gtk4 >= 4.20` in) and uploads it as the `cordial-deb` artifact,
   exactly as it did before this change. Nothing in that file was touched.
2. **`apt.yml`**, new, triggers when that job's workflow finishes, downloads
   `cordial-deb` from that specific run, and runs `build-repo.sh` against
   it -- but only when `APT_GPG_PRIVATE_KEY` is set; otherwise it stops
   after saying so. It does not rebuild Cordial from source: the whole point
   of reading `release.yml`'s artifact is not compiling the workspace a
   second time on every push. Its output is a plain `actions/upload-artifact`
   named `cordial-apt-repo`, not a Pages deployment.
3. **`flatpak.yml`** is the one workflow that deploys to GitHub Pages, and
   deliberately stays the only one: Pages, published through
   `actions/deploy-pages`, is one deployment for the whole site with no
   per-path incremental publish, so a second workflow calling it
   independently would not add an `/apt/` directory to the site, it would
   *replace* the site with one. `flatpak.yml`'s "Assemble the Pages tree"
   step now looks up `apt.yml`'s latest successful run, downloads
   `cordial-apt-repo` from it, and copies it into `pages/apt/` alongside the
   existing `pages/repo/` (the OSTree remote) and `pages/cordial.flatpakrepo`.
   If no successful `apt.yml` run exists yet -- no key, or it has simply
   never run -- that step says so and publishes without `/apt/`, the same
   graceful-degradation shape the existing "is `site/` there" check already
   uses.

**The consequence worth knowing rather than being surprised by:** because
step 3 reads `apt.yml`'s *latest* run rather than one from the same commit,
the apt tree in a given Pages deploy can lag the flatpak repo beside it by
up to one push. The two workflows run independently and neither blocks on
the other finishing mid-run. That is a deliberate trade against a much
larger change -- merging the two workflows so one job assembles both trees
in a single run -- to a file (`flatpak.yml`) that has already cost real
debugging time to get right (its own comments record three separate
Pages-specific traps found the hard way). If the lag is ever worth
removing, that merge is the fix; it has not been attempted here.

### Installing it, and verifying the key

The README's install section (§2.3) carries the commands a user runs. The
short version:

```bash
sudo curl -fsSL https://luohoa97.github.io/cordial/apt/cordial-archive-keyring.gpg \
    -o /etc/apt/keyrings/cordial-archive-keyring.gpg
echo "deb [signed-by=/etc/apt/keyrings/cordial-archive-keyring.gpg] https://luohoa97.github.io/cordial/apt stable main" \
    | sudo tee /etc/apt/sources.list.d/cordial.list
sudo apt update && sudo apt install cordial
```

That is the modern form -- a key named on the `deb` line by path, not
`apt-key add`, which Debian deprecated for exactly the reason a keyring
scoped to one repository is safer than one every repository on the system
shares.

### The key

**No key exists yet.** Once `APT_GPG_KEY_ID` is set, this section is where
its fingerprint goes, published out of band from the install commands above
so a user has a second source to check against:

```bash
gpg --show-keys --with-fingerprint /etc/apt/keyrings/cordial-archive-keyring.gpg
```

Until this section names a fingerprint, there is nothing at the published
URL to check it against in the first place -- see "Nothing is signed yet" in
the README.

### Key rotation

**A user who already has the keyring file keeps trusting the old key until
they replace it, which is the correct failure mode.** `apt` checks the
*current* `InRelease` signature against whatever is in
`/etc/apt/keyrings/cordial-archive-keyring.gpg` on their machine, on every
`apt update`. If the signing key is ever rotated -- planned renewal at
expiry, or a compromise -- new `Release` files signed by the new key will
not verify against a keyring file that still holds the old one, and `apt
update` starts failing loudly with a signature error rather than silently
accepting either key. That is safe, and it is also exactly the support
question "why did apt stop updating" that a release note needs to answer
the day it happens: the fix is re-running the `curl` line above to fetch the
new keyring, the same shape the Flatpak side's "re-add the remote" note
already gives users for the same underlying reason -- **a local copy of
trust material does not update itself, and pretending otherwise is how a
rotation goes unnoticed until someone files a bug.**

### What this does and does not fix

Signing proves the `Release` file -- and everything chained under it -- was
produced by whoever holds the private key. It does not prove that person is
trustworthy, and it is a different guarantee from Debian's own archive,
which additionally reviews what a package *does*. What it fixes is the gap
that exists without it: today, anyone who can write to the GitHub Pages site
can serve a different `.deb` under the same name with no warning to an
installed client, addressed the same way
[flatpak-remote-signing.md](flatpak-remote-signing.md) frames it for the
OSTree remote.

## §2. Official Debian: an honest assessment

**Not started, and the case for starting it is weaker than it looks.** This
is not a decision the packaging in this repository can make on its own --
it depends on a Debian Developer choosing to sponsor it, and on ftp-master
review whose outcome nobody here controls -- so what follows is an
assessment to decide *whether it is worth asking someone*, not a plan.

**The process itself.** Debian does not accept a package directly from an
upstream project. It needs:

- An ITP (Intent To Package) bug against `wnpp`, so the intent is public and
  duplicate work is avoided.
- `debian/` packaging that satisfies Debian Policy -- a different shape from
  everything in `packaging/` today, which is hand-built `.deb`/`.rpm`/Arch
  trees rather than a `debian/rules`+`debhelper` source package, for the
  reasons `packaging/deb/build-deb.sh`'s own header gives (a virtual Cargo
  workspace with no single `[package]`, which is what already ruled out
  `cargo-deb` and `dpkg-buildpackage` for the `.deb` this repository builds
  today).
- A sponsor: an existing Debian Developer or Debian Maintainer who reviews
  the packaging and uploads it, because Cordial has none today. Finding one
  is itself work, not a formality.
- ftp-master review of the *upload*, separately from the sponsor's own
  review, which is where DFSG-freeness and archive-section questions
  (below) actually get decided.

Realistic timeline for a new package going through this correctly, even
with an engaged sponsor, is months rather than weeks -- this is well short
of a criticism of the process; it is a review step this repository's own
"never state an unobserved result" culture would recognise, applied by
people outside it who cannot take Cordial's own claims about itself on
faith either.

**Licensing is not the obstacle.** Cordial itself is GPL-3.0-or-later, which
is DFSG-free without qualification. The vendored third-party code
[`NOTICE`](../../NOTICE) and
[`THIRD-PARTY-NOTICES.md`](../../THIRD-PARTY-NOTICES.md) list --
libbadcpu (MIT), mcpelauncher-linker (MIT, with the AOSP NOTICE it carries),
libjnivm (MIT), mocktail-webview (Apache-2.0) -- are each independently
DFSG-free too. None of that is what would slow a review down.

**What would: Cordial's entire purpose is loading a proprietary binary it
fetches at runtime, and Debian has a section for exactly that distinction.**
Cordial ships no Roblox code -- see AGENTS.md's "Permanently out of scope" --
but the software has no purpose without one: on first run it offers to fetch
Roblox's official Android build, and does nothing useful until it has. That
is a materially different shape from, say, `wine`, which is general-purpose
and does not depend on any specific proprietary program existing to be
useful. Debian's own `main` archive is reserved for software that neither
requires nor recommends anything outside `main`; software whose purpose
depends on non-free software it does not itself ship is the textbook case
for the `contrib` section instead. Games and compatibility launchers built
around one specific proprietary target -- the general shape Cordial is in --
have historically landed in `contrib`, stayed out of the archive entirely
and lived in third-party repositories, or taken a long review to place, more
often than they have gone straight into `main`. **This is a reasoned
expectation, not a ruling** -- only ftp-master review actually decides it,
and this document does not have that authority and should not be read as
though it does.

**What is not a Debian-specific obstacle, for the avoidance of a wrong
inference from the Flathub section of the README:** Flathub's
generative-AI policy, which is the reason Cordial is not on Flathub today,
is Flathub's own policy and there is no known equivalent in Debian's own
process. That obstacle does not carry over. It is named here only so nobody
reads "blocked from one channel over AI-assisted code" and assumes it
applies to every channel; there is no evidence it does.

**The honest conclusion:** an ITP is realistic to *file* at any point --
that costs one bug report and starts the clock on visibility -- but landing
in `main` is not the likely outcome even with a willing sponsor, and
`contrib`, if that is where it lands, still requires a user to have already
enabled non-free-adjacent sections and does not solve the "one line adds a
repository" problem this document's §1 already solves today. **§1 is
therefore the plan, not a stopgap for §2** -- the same framing
[flatpak-remote-signing.md](flatpak-remote-signing.md) gives for Flathub, and
for the same underlying reason: the channel Cordial controls is the one that
can actually ship this week.
