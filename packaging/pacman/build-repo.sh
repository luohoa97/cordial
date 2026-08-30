#!/usr/bin/env bash
# Build a signed pacman repository database from one or more already-built
# .pkg.tar.zst files.
#
# This is Cordial's own repository -- the analogue of packaging/apt/build-repo.sh
# and packaging/rpm/build-repo.sh for `pacman -S` rather than `apt`/`dnf`
# install. It is not packaging/aur/cordial/PKGBUILD's concern and does not
# touch it: that directory builds the package makepkg produces (release.yml's
# `arch` job does that, and syncs the PKGBUILD to a separate packaging
# repository for AUR-shaped consumption); this script starts after that
# package already exists, the same relationship packaging/rpm/build-repo.sh
# has to packaging/rpm/build-rpm.sh. See docs/design/pacman-repository.md for
# what this is, why AUR is a different question with its sign-ups currently
# closed, and the procedure for the key.
#
# Usage:
#     packaging/pacman/build-repo.sh [--outdir DIR] [--allow-unsigned] PKG [PKG...]
#
# Signing is driven by ARCH_GPG_KEY_ID, the fingerprint of a secret key
# already present in the calling GNUPGHOME -- the same division
# packaging/apt/build-repo.sh and packaging/rpm/build-repo.sh keep between
# importing a key (CI's job, via "Import the signing key") and signing with
# one that is already there (this script's job). Without ARCH_GPG_KEY_ID set,
# this script refuses outright, for the same reason the other two do: a
# repository with every file pacman needs to install from it except the one
# that makes it trustworthy is a worse thing to publish than nothing.
# --allow-unsigned overrides the refusal for inspecting the tree's shape
# locally; publishing what it produces is the one thing that flag does not
# make acceptable.
#
# Needs: repo-add (from pacman-contrib, or pacman itself on Arch), gpg.
# **repo-add's exact flags here are INFERRED against its documented CLI, not
# run** -- repo-add is an Arch-only tool and this repository's own
# development host is Fedora Silverblue, which has no pacman on it and no
# room on disk to gain one. .github/workflows/pacman.yml runs this inside
# archlinux:base-devel specifically so this gets exercised for real the first
# time it runs there, the same way release.yml's own `arch` job already
# builds packaging/aur/cordial/PKGBUILD in that image rather than trying to
# cross-build Arch packages anywhere else; if this script is wrong, that is
# where it will show, not here.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=$(git -C "$here" rev-parse --show-toplevel)

# Fixed rather than a flag, matching packaging/apt/build-repo.sh's ARCH: this
# repository ships one architecture today. Widen the day a second one exists.
DBNAME=cordial
ARCH=x86_64

outdir="$repo/dist/pacman-repo"
allow_unsigned=0
pkgs=()
while [ $# -gt 0 ]; do
    case "$1" in
        --outdir) outdir=$2; shift 2 ;;
        --allow-unsigned) allow_unsigned=1; shift ;;
        --) shift; break ;;
        -*) echo "unknown argument: $1" >&2; exit 2 ;;
        *) pkgs+=("$1"); shift ;;
    esac
done
pkgs+=("$@")

if [ "${#pkgs[@]}" -eq 0 ]; then
    echo "usage: $(basename "$0") [--outdir DIR] [--allow-unsigned] PKG [PKG...]" >&2
    exit 2
fi
for pkg in "${pkgs[@]}"; do
    [ -f "$pkg" ] || { echo "error: $pkg does not exist" >&2; exit 2; }
done

# The refusal this script exists to make. An empty ARCH_GPG_KEY_ID and a
# missing one are treated alike -- a secret set to the empty string is not a
# key either.
if [ -z "${ARCH_GPG_KEY_ID:-}" ] && [ "$allow_unsigned" -ne 1 ]; then
    cat >&2 <<'EOF'
error: ARCH_GPG_KEY_ID is not set.

This script refuses to build a pacman repository with no signature, for the
same reason packaging/apt/build-repo.sh and packaging/rpm/build-repo.sh
refuse: a repository database with no signature still installs, silently,
for anyone whose pacman.conf does not insist otherwise -- and this script is
not going to be the thing that makes that the normal path.

Import a signing key into this GNUPGHOME first (see
docs/design/pacman-repository.md for the full procedure) and set
ARCH_GPG_KEY_ID to its fingerprint, or pass --allow-unsigned to build the
tree anyway, for checking the layout on a machine with no key. A tree built
with --allow-unsigned is for your own inspection; publishing it is the one
thing --allow-unsigned does not make acceptable.
EOF
    exit 1
fi
if [ "$allow_unsigned" -eq 1 ] && [ -n "${ARCH_GPG_KEY_ID:-}" ]; then
    echo "ARCH_GPG_KEY_ID is set; ignoring --allow-unsigned and signing anyway" >&2
    allow_unsigned=0
fi
if [ "$allow_unsigned" -eq 1 ]; then
    echo "::warning::--allow-unsigned: building an UNSIGNED pacman repository. Do not publish this tree." >&2
fi

command -v repo-add >/dev/null 2>&1 || { echo "error: repo-add is not installed (pacman-contrib)" >&2; exit 1; }

mkdir -p "$outdir/$ARCH"
outdir=$(cd "$outdir" && pwd)
destdir="$outdir/$ARCH"

echo "==> copying ${#pkgs[@]} package(s) into $destdir"
for pkg in "${pkgs[@]}"; do
    install -m644 "$pkg" "$destdir/$(basename "$pkg")"
    echo "    $(basename "$pkg")"
done

dbfile="$destdir/$DBNAME.db.tar.gz"

# repo-add's own defaults name the files-database identically to the
# db-database but with .files.tar.gz -- both are written from one invocation,
# which is why there is only one repo-add call here rather than a matching
# pair the way createrepo_c's Packages/Release are two separate steps.
if [ "$allow_unsigned" -eq 1 ]; then
    echo "==> repo-add (unsigned)"
    ( cd "$destdir" && repo-add "$dbfile" ./*.pkg.tar.zst )
else
    echo "==> repo-add -s -k $ARCH_GPG_KEY_ID"
    # -s signs the resulting database (both cordial.db.tar.gz and
    # cordial.files.tar.gz get a detached .sig alongside them); -k pins which
    # secret key repo-add's own gpg call signs with, the same reason
    # packaging/apt/build-repo.sh's gpg invocations always pass
    # --local-user rather than relying on a homedir default. **Individual
    # .pkg.tar.zst are deliberately never signed here**: makepkg's own
    # `--sign` (or `-p`) would do that, but it would rewrite nothing about
    # the package file itself, only add a sidecar .sig -- so unlike rpm
    # --addsign, package-level signing does not actually conflict with
    # cosign's release-page signature covering the same bytes. It is left
    # out anyway, for the same reason gpgcheck=0 stays permanent in
    # packaging/cordial.repo: this design signs the index once
    # (cordial.db.tar.gz) rather than every artefact it lists, the same
    # shape apt's Release and dnf's repomd.xml already take, so pacman.conf's
    # SigLevel only has to trust the database, never the package.
    ( cd "$destdir" && repo-add -s -k "$ARCH_GPG_KEY_ID" "$dbfile" ./*.pkg.tar.zst )

    # The public half, for a user's pacman-key. Armoured, matching
    # packaging/rpm/build-repo.sh's RPM-GPG-KEY-cordial rather than apt's
    # binary export: `pacman-key --add` reads the armoured form directly.
    gpg --batch --yes --armor --export "$ARCH_GPG_KEY_ID" > "$outdir/cordial-archive-keyring.asc"
fi

echo "==> built: $outdir"
find "$outdir" -type f | sort
