#!/usr/bin/env bash
# Build a signed dnf/yum repository from one or more already-built .rpm files.
#
# This is Cordial's own repository -- the analogue of packaging/apt/build-repo.sh
# for `dnf install` rather than `apt install`, and of packaging/cordial.flatpakrepo
# for a channel with no single downloadable remote definition that embeds the
# key itself. It does not build Cordial and does not build an .rpm;
# packaging/rpm/build-rpm.sh already does that (by way of make-srpm.sh), and
# this script's whole job starts after that one's is finished. See
# docs/design/rpm-repository.md for what this is, what it deliberately is
# not (submission to Fedora's own repos), and the procedure for the key.
#
# Usage:
#     packaging/rpm/build-repo.sh [--outdir DIR] [--allow-unsigned] RPM [RPM...]
#
# Signing is driven by RPM_GPG_KEY_ID, the fingerprint of a secret key already
# present in the calling GNUPGHOME -- this script never imports a key itself,
# the same division apt.yml's "Import the signing key" step and
# packaging/apt/build-repo.sh keep between them. Without RPM_GPG_KEY_ID set,
# this script refuses outright, for the same reason packaging/apt/build-repo.sh
# does: a repository that has every file dnf needs to install from it except
# the one that makes it trustworthy is a worse thing to publish than nothing,
# because it still *works*, silently, for anyone willing to add
# `gpgcheck=0` -- and unlike apt, which needs a user to type `[trusted=yes]`
# to get there, a dnf .repo file with `gpgcheck=0` baked in asks for no such
# admission from whoever downloads it. That asymmetry is argued at length in
# docs/design/rpm-repository.md; the short version is that it makes the
# unsigned case *more* dangerous here, not less, so this script draws the
# same line apt's does rather than a softer one. --allow-unsigned overrides
# the refusal for inspecting the tree's shape locally; publishing what it
# produces is the one thing that flag does not make acceptable.
#
# Needs: createrepo_c, gpg. createrepo_c ships from Debian/Ubuntu's
# createrepo-c package and from Fedora's own createrepo_c package; neither is
# on this repository's own development host (Fedora Silverblue with no
# createrepo_c on the base image and no room on disk for an rpm-ostree layer
# to add one -- see AGENTS.md's note on gdb for the same class of constraint),
# so **the createrepo_c invocations in this script are INFERRED against its
# documented CLI, not run**. CI's ubuntu-latest runner installs createrepo-c
# specifically so this gets exercised for real the first time it runs there;
# if it is wrong, that is where it will show, not here.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=$(git -C "$here" rev-parse --show-toplevel)

outdir="$repo/dist/rpm-repo"
allow_unsigned=0
rpms=()
while [ $# -gt 0 ]; do
    case "$1" in
        --outdir) outdir=$2; shift 2 ;;
        --allow-unsigned) allow_unsigned=1; shift ;;
        --) shift; break ;;
        -*) echo "unknown argument: $1" >&2; exit 2 ;;
        *) rpms+=("$1"); shift ;;
    esac
done
rpms+=("$@")

if [ "${#rpms[@]}" -eq 0 ]; then
    echo "usage: $(basename "$0") [--outdir DIR] [--allow-unsigned] RPM [RPM...]" >&2
    exit 2
fi
for rpm in "${rpms[@]}"; do
    [ -f "$rpm" ] || { echo "error: $rpm does not exist" >&2; exit 2; }
done

# The refusal this script exists to make. An empty RPM_GPG_KEY_ID and a
# missing one are treated alike -- a secret set to the empty string is not a
# key either.
if [ -z "${RPM_GPG_KEY_ID:-}" ] && [ "$allow_unsigned" -ne 1 ]; then
    cat >&2 <<'EOF'
error: RPM_GPG_KEY_ID is not set.

This script refuses to build a dnf/yum repository with no signature. Unlike
apt, which needs a user to add `[trusted=yes]` to opt into an unsigned
repository, a dnf .repo file can carry `gpgcheck=0` baked in with nothing for
the person adding it to notice -- so an unsigned tree here is not a weaker
version of the real thing, it is a silent one, and this script is not going
to be the thing that produces it by default.

Import a signing key into this GNUPGHOME first (see
docs/design/rpm-repository.md for the full procedure -- it mirrors
docs/design/apt-repository.md's) and set RPM_GPG_KEY_ID to its fingerprint,
or pass --allow-unsigned to build the tree anyway, for checking the layout
on a machine with no key. A tree built with --allow-unsigned is for your own
inspection; publishing it is the one thing --allow-unsigned does not make
acceptable.
EOF
    exit 1
fi
if [ "$allow_unsigned" -eq 1 ] && [ -n "${RPM_GPG_KEY_ID:-}" ]; then
    echo "RPM_GPG_KEY_ID is set; ignoring --allow-unsigned and signing anyway" >&2
    allow_unsigned=0
fi
if [ "$allow_unsigned" -eq 1 ]; then
    echo "::warning::--allow-unsigned: building an UNSIGNED dnf repository. Do not publish this tree." >&2
fi

command -v createrepo_c >/dev/null 2>&1 || { echo "error: createrepo_c is not installed" >&2; exit 1; }

mkdir -p "$outdir"
outdir=$(cd "$outdir" && pwd)

# One directory per Fedora release, so that `baseurl=.../rpm/$releasever/$basearch`
# resolves for whichever release actually has a build in it and 404s for any
# that does not, rather than a bare directory serving one release's .rpm to
# every dnf version that asks. This is deliberately not a merged "latest wins"
# tree: release.yml builds inside registry.fedoraproject.org/fedora:44 today
# (packaging/rpm/build-rpm.sh's own header explains why -- gtk4 4.22 and
# libadwaita 1.9, which Fedora 43 lacks), so as of this writing this script
# only ever populates a "44" directory, and a dnf on Fedora 43 or 45 gets a
# clean 404 from the baseurl rather than an fc44 binary it did not ask for.
# See docs/design/rpm-repository.md for the fuller argument -- a 404 here is
# the correct failure, not a gap to paper over with a "latest" symlink.
#
# The release number is read from each .rpm's own filename rather than
# invoked against the package with `rpm` -- there is no `rpm` binary on this
# repository's own apt-based development host either, and release.yml's own
# naming convention (packaging/rpm/make-srpm.sh's %dist tag, e.g.
# cordial-0.12.1-1.fc44.x86_64.rpm) already carries the answer in the name a
# real rpmbuild wrote, so parsing it here needs nothing this script does not
# already have.
echo "==> sorting ${#rpms[@]} package(s) by Fedora release"
declare -A dirs_seen=()
for rpm in "${rpms[@]}"; do
    base=$(basename "$rpm")
    if [[ ! "$base" =~ \.fc([0-9]+)\. ]]; then
        echo "error: $base has no .fcNN. release tag in its filename -- refusing to guess which /rpm/<releasever>/ directory it belongs in" >&2
        exit 1
    fi
    releasever="${BASH_REMATCH[1]}"
    arch="${base%.rpm}"
    arch="${arch##*.}"

    destdir="$outdir/$releasever/$arch"
    mkdir -p "$destdir"
    install -m644 "$rpm" "$destdir/$base"
    dirs_seen["$destdir"]=1
    echo "    $base -> rpm/$releasever/$arch/"
done

echo "==> running createrepo_c over $(( ${#dirs_seen[@]} )) release/arch director$([ "${#dirs_seen[@]}" -eq 1 ] && echo y || echo ies)"
for d in "${!dirs_seen[@]}"; do
    # --update would only touch entries newer than what is already in
    # repodata/, and pool contents here are only ever added, never edited in
    # place, so a plain (re)generation each run is simpler than reasoning
    # about --update's staleness rules and costs nothing extra: this script
    # is handed the whole set of .rpm for a release, not an incremental one.
    createrepo_c "$d"
done

if [ "$allow_unsigned" -eq 1 ]; then
    echo "==> --allow-unsigned: not signing any repomd.xml, and not exporting a public key"
else
    echo "==> signing with $RPM_GPG_KEY_ID"
    # One signature per release/arch directory's own repomd.xml, the same
    # shape apt's Release/InRelease takes: repomd.xml's own manifest already
    # carries the checksum of primary.xml.gz, and primary.xml.gz carries the
    # checksum of every .rpm createrepo_c indexed, so one signature roots a
    # hash chain that covers the lot. **Individual .rpm are deliberately
    # never re-signed here** (no `rpm --addsign`): doing that would rewrite
    # the package's own header and produce a different file at this URL than
    # the one release.yml built and release.yml's own cosign step already
    # signed on the release page, for no security this design does not
    # already get from signing repomd.xml. gpgcheck therefore stays 0 in
    # packaging/cordial.repo permanently, not as a placeholder for later --
    # see that file's own comment.
    for d in "${!dirs_seen[@]}"; do
        gpg --batch --yes --local-user "$RPM_GPG_KEY_ID" \
            --detach-sign --armor -o "$d/repodata/repomd.xml.asc" "$d/repodata/repomd.xml"
    done

    # The public half, once for the whole tree rather than once per release
    # directory -- it is the same key regardless of which Fedora release a
    # user's dnf is asking for. Exported armoured (`--armor`), which is the
    # form every RPM-GPG-KEY-* file ships in on a real Fedora install (unlike
    # apt's `signed-by=`, which wants the binary form) -- `pacman-key --add`
    # and `rpm --import` both read the armoured form directly, and it is
    # readable in a browser if anyone opens the URL out of curiosity.
    gpg --batch --yes --armor --export "$RPM_GPG_KEY_ID" > "$outdir/RPM-GPG-KEY-cordial"
fi

echo "==> built: $outdir"
find "$outdir" -type f | sort
