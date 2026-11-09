# ADR-067: Compiler caches are launchers, not a build requirement

**Status:** accepted
**Date:** 2026-11-09

## Context

Cordial's build is two compilers and a lot of both. Cargo compiles the Rust
workspace into its own `target/`; two `build.rs` files then configure CMake for
the bionic linker shim and, behind the `vr` feature, dynarmic. Cargo's directory
is incremental, so the ordinary edit loop is already cheap. Everything else
starts from nothing: `cargo clean`, a fresh checkout, the extra target
directories the repository already keeps for toolbox/distrobox/Nix builds, and
CI.

`ccache` and `sccache` both implement the same idea for that case — hash the
compiler, the flags and the translation unit, keep the object, hand it back when
the same one is asked for again. Before this ADR the repository knew about
neither. On the machine this was measured on, `ccache` happened to work through
`/usr/lib/ccache` symlinks earlier in `PATH`, which is invisible in the tree and
changes which `clang` CMake records in its cache; on any other machine the same
`cargo build --release` compiled the same C++ again.

The two tools are not interchangeable by choice: `ccache` speaks C and C++, and
`sccache` speaks rustc as well as C/C++, so the natural division is one cache per
language family rather than both racing for the same objects.

## Decision

Both caches are **discovered, never required**. A checkout with neither
installed compiles exactly what it compiled before this change.

- **Rust.** `.cargo/config.toml` sets
  `build.rustc-wrapper = "./tools/rustc-wrapper.sh"`. That script execs
  `sccache "$@"` when `sccache` is on `PATH`, and `exec "$@"` (the real rustc)
  when it is not. Naming `sccache` directly in the config would fail the build on
  every host without it, which is most of them.
- **Native.** `cmake/CompilerCache.cmake` is included by `native/CMakeLists.txt`
  and `crates/cordial-guest/native/CMakeLists.txt`. It sets
  `CMAKE_C_COMPILER_LAUNCHER` and `CMAKE_CXX_COMPILER_LAUNCHER` to `ccache` when
  found, else `sccache`, else the empty string. The variables are set on every
  configure, so a `CMakeCache.txt` from a run that had a cache cannot keep
  pointing at a tool that has since been removed. The launcher is inherited by
  every target under the two projects, the bionic linker and dynarmic included.
- **Escape hatches.** `CORDIAL_COMPILER_CACHE=off` turns both off without
  changing the compiler; `CORDIAL_NATIVE_CACHE=<tool>` names a launcher
  explicitly for an `sccache` with a remote backend or for a `ccache` outside
  `PATH`.
- **Not in packaging or CI.** The Flatpak SDK, the Nix build and the distro
  packages keep compiling from scratch. None of them installs either tool, and
  the guarded launcher means they do not have to. CI is a separate question,
  because its runners are ephemeral and a disk cache dies with them: a useful CI
  cache is a remote backend with storage and credentials, which is a decision of
  its own rather than a flag in this one.

## Measurements

Three cold `cargo build --release` runs of the same tree on the same target
path, each after `rm -rf target`: one with `CORDIAL_COMPILER_CACHE=off`, one
filling empty caches, one against what the previous run filled. 12 cores, Rust
1.99.1, Clang 23.1.4, the repository's `[profile.release]` with `debug = true`,
on a desktop that was also running a browser, so the runs repeat to about ±10 %.

| arm | wall clock | rustc (184 invocations) | native (106 objects) |
|---|---|---|---|
| no cache | 120 s | — | — |
| cache filling | 135–149 s (3 runs) | 2 hits, 179 misses | 1 hit, 105 misses |
| cache warm | 34–36 s (2 runs) | 180 hits, 1 miss (99.4 %) | 106 hits, 0 misses |

The warm build is 3.3–3.5× faster than the same build with no cache. An earlier
pair, taken while the machine was busiest and discarded for the table, measured
136 s against 34 s (4.0×) — the 3.3× end is the conservative one. Filling costs
about 15–30 s on the first cold build — the cache writes, 268 MB of rustc output
and 4.7 MB of native objects — and is paid once per compiler, flag and input
combination.

One rustc invocation misses on a warm build and was not identified: nothing in
the tree changed between the runs, so it is a build script whose output is not
reproducible or an input sccache hashes differently. At one crate in 184 it was
not worth chasing.

The default 10 GiB sccache limit is nowhere near full at 268 MB, so the
measurement needed no sizing; ccache's 4.7 MB likewise.

## What this does not claim

- That the ordinary edit loop gets faster. It does not: Cargo's own incremental
  build reuses the artifacts before any wrapper is involved, and `sccache`
  deliberately passes incremental compilations through without caching. The
  benefit is the cold build — `cargo clean`, a new checkout, a second target
  directory, and the release profile the README documents.
- That every checkout benefits equally. A cache only hits when the compiler,
  version, flags and inputs match; a toolchain update or a changed Cargo feature
  set is a miss by design.
- That the container recipes benefit without installing the tools inside the
  container. `just build toolbox` runs cargo against the mounted tree from
  inside the container, so discovery happens against that container's `PATH`;
  a cache installed only on the host is not seen, and the build simply does not
  cache. The same is true of the Flatpak SDK sandbox.
- Anything about a shared or remote cache. Both tools support one, neither is
  configured here, and a cache that crosses a network is worth its own threat
  model — compiler output is executable code.
- That `cargo test --workspace` is cheaper on a second run. Test binaries are
  as cacheable as any other rustc invocation, but the tests themselves run every
  time and dominate.

## Alternatives

**Make `sccache` a hard `rustc-wrapper` (or `ccache` a CMake requirement).**
Rejected: it fails the build on every host without the tool, including the
Flatpak SDK, the Nix sandbox and the Fedora CI image. The whole point is that
the tool is optional.

**Only wire it into `just build`.** Rejected: `cargo build --release` is the
command CONTRIBUTING documents and what CI runs. A cache that only exists behind
a wrapper script people bypass is the "check that only runs when asked for"
shape this repository already names as a bug.

**Use `sccache` for the native tree too.** It works and is the fallback when
`ccache` is absent, but on a local disk `ccache` is faster for C/C++, and this
machine already had it. Measuring both against the same native tree is a change
of its own if the fallback ever becomes the common case.

**A remote cache for CI.** Not rejected, just deferred. It needs a backend, a
key-storage story and a decision about trusting compiler output produced on a
shared host; the local launchers do not get in its way.
