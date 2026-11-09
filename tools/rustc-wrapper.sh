#!/bin/sh
# Cargo's rustc wrapper, named by .cargo/config.toml.
#
# Runs sccache when one is installed and rustc when one is not. That is the
# whole reason this is a script instead of `rustc-wrapper = "sccache"`: Cargo
# fails the build outright when the named wrapper is missing, and a checkout
# without sccache has to build exactly as it did before this existed. The
# Flatpak, the Nix build and the distro packages all fall in that group.
#
# CORDIAL_COMPILER_CACHE=off skips the cache without changing the compiler; it
# is how the control arm of the measurement in docs/adr/ADR-067 was taken.
#
# The cache is Cargo's second layer, not its first. An ordinary incremental
# build never asks sccache to cache anything -- Cargo reuses its own artifacts
# and sccache passes incremental compilations through -- so the payoff is
# after `cargo clean`, on a fresh checkout, or in a second target directory.
if [ "${CORDIAL_COMPILER_CACHE:-on}" != "off" ] && command -v sccache >/dev/null 2>&1; then
    exec sccache "$@"
fi
exec "$@"
