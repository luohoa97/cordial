#!/usr/bin/env bash
# Derive a *package* version from `git describe`, and print it as shell
# assignments.
#
# **This is not where the window title's version comes from, and this comment
# used to say it was.** It claimed AGENTS.md named `git describe --tags
# --always --dirty` as the source for both; the title's version is
# `Cargo.toml`'s and has been since crates/cordial-shell/build.rs stopped
# computing one. What that build script still takes from here is
# `CORDIAL_GIT_SHA`, the provenance half, which the three native package
# scripts export from CORDIAL_SHORTHASH below.
#
# A package version needs something the manifest cannot give it: dpkg, rpm and
# pacman all compare versions to decide whether an upgrade is an upgrade, so
# two builds cut between one tag and the next must order, and a bare 0.14.0
# repeated does not. That is what the distance-from-tag component is for, and
# it is why this file keeps deriving from `git describe` rather than reading
# the manifest.
#
# Printed rather than exported, so this is meant to be evaluated rather than
# sourced or run for effect:
#
#     eval "$(packaging/version.sh)"
#     echo "$CORDIAL_VERSION"
#
# That is a deliberate choice over `source`-ing it: a sourced script that
# forgets to `export` leaves the caller with nothing and no error, and one
# that changes `set -e` underneath a caller's own shell options is a footgun
# for exactly the reason this project's own justfile avoids it. `eval` fails
# loudly if this script fails, because a broken command substitution with
# `set -e` upstream stops the pipeline.
#
# packaging/rpm/make-srpm.sh carries its own copy of this derivation rather
# than calling this script. That script already works and is exercised by
# Copr builds; reworking it to share this file was judged not worth the risk
# to something that already ships. If you change the transform here, check
# whether make-srpm.sh's needs the same fix -- they are two implementations of
# one idea and can drift.
#
# Sets:
#   CORDIAL_DESCRIBE   the raw `git describe --tags --long` string with the
#                      leading v stripped, e.g. 0.7.0-37-gcbd53e5. Used to name
#                      the AppImage and to build the .deb and .rpm versions.
#                      It once fed a CORDIAL_BUILD_VERSION that no longer
#                      exists; nothing reads that variable now.
#   CORDIAL_VERSION    the tag alone, e.g. 0.7.0
#   CORDIAL_COMMITS    commits since that tag; 0 at an exact tag
#   CORDIAL_SHORTHASH  the abbreviated commit, e.g. cbd53e5
set -euo pipefail

describe=$(git describe --tags --long --abbrev=7)
case "$describe" in
    v*) ;;
    *)
        echo "expected a v-prefixed tag reachable from HEAD, got '$describe' -- fetch tags (git fetch --tags) or check out a release" >&2
        exit 1
        ;;
esac

version=${describe#v}; version=${version%%-*}
rest=${describe#v${version}-}
commits=${rest%%-*}
shorthash=${rest#*-g}

printf 'CORDIAL_DESCRIBE=%s\n' "${describe#v}"
printf 'CORDIAL_VERSION=%s\n' "$version"
printf 'CORDIAL_COMMITS=%s\n' "$commits"
printf 'CORDIAL_SHORTHASH=%s\n' "$shorthash"
