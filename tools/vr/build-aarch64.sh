#!/usr/bin/env bash
# Cross-compile cordial-run for aarch64 Linux from an x86-64 host.
#
#   tools/vr/build-aarch64.sh            # aarch64, into target-aarch64/
#   ARCH=x86_64 tools/vr/build-aarch64.sh  # same recipe for x86-64, into target/
#   ARCH=x86_64 BINS="cordial-run cordial-shell" tools/vr/build-aarch64.sh
#                                          # the launcher as well as the client
#   ARCH=x86_64 tools/vr/build-aarch64.sh cargo test -p cordial-guest --features dynarmic
#                                          # any cargo subcommand, same environment
#
# The build turns on `cordial-runtime/vr`, since VR is what this script is
# for; a cargo subcommand gets only the features it is given.
#
# The compilers are the host's (clang, lld, rustup's aarch64 std); only the
# headers and libraries come from an Ubuntu 24.04 filesystem built from
# tools/vr/Dockerfile.sysroot and `docker export`ed. That image is built under
# qemu-user once and then reused, so the slow emulated step is apt, not the
# compile. The x86_64 mode exists because a host without GTK4/libadwaita
# development packages cannot otherwise check that the x86-64 build still
# compiles, and it is the same sysroot recipe on the other architecture.
#
# The target directories are this fork's own and must never be another
# checkout's: two builds sharing one target/ produced rlibs missing symbols
# that were plainly in the source (AGENTS.md, "Build and test").
set -euo pipefail

ARCH=${ARCH:-aarch64}
repo=$(cd "$(dirname "$0")/../.." && pwd)
case "$ARCH" in
    aarch64) platform=linux/arm64; triple=aarch64-linux-gnu
             default_target_dir="$repo/target-aarch64" ;;
    x86_64)  platform=linux/amd64; triple=x86_64-linux-gnu
             default_target_dir="$repo/target" ;;
    *) echo "ARCH must be aarch64 or x86_64, not $ARCH" >&2; exit 2 ;;
esac
rust_target="$ARCH-unknown-linux-gnu"
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$default_target_dir}
sysroot=${SYSROOT:-$repo/target-aarch64/sysroot-$ARCH}
image="cordial-sysroot-$ARCH:24.04"

# Built every time rather than only when missing: an unchanged Dockerfile is a
# layer-cache hit in about a second, and a changed one (mesa-vulkan-drivers was
# added after the first image existed) would otherwise never reach the sysroot.
docker build -q --platform "$platform" -f "$repo/tools/vr/Dockerfile.sysroot" \
    -t "$image" "$repo/tools/vr" >/dev/null

# Re-exported whenever the image is newer than the stamp, so a changed package
# list reaches the sysroot rather than leaving a stale one in place.
image_id=$(docker image inspect -f '{{.Id}}' "$image")
if [ "$(cat "$sysroot/.image-id" 2>/dev/null)" != "$image_id" ]; then
    rm -rf "$sysroot"
    mkdir -p "$sysroot"
    cid=$(docker create --platform "$platform" "$image" /bin/true)
    # Device nodes and setuid bits are neither needed nor extractable without
    # root, and tar's refusal of them is not an error worth stopping for.
    docker export "$cid" | tar -x -C "$sysroot" \
        --exclude='dev/*' --no-same-owner --no-same-permissions 2>/dev/null || true
    docker rm "$cid" >/dev/null
    echo "$image_id" > "$sysroot/.image-id"
fi
# Docker leaves empty placeholders for the three files it bind-mounts into a
# running container. The sysroot doubles as qemu-user's `-L` prefix when the
# aarch64 client runs, and qemu opens the prefixed path first, so an empty
# resolv.conf there meant every lookup went to 127.0.0.1:53 and failed with
# "Temporary failure in name resolution". Without them the host's are read.
rm -f "$sysroot/etc/resolv.conf" "$sysroot/etc/hosts" "$sysroot/etc/hostname"

rustup target add "$rust_target" >/dev/null

# CMake gets the sysroot from a toolchain file rather than from CFLAGS alone:
# native/CMakeLists.txt finds PipeWire's headers with find_path when
# pkg-config does not answer, and without CMAKE_SYSROOT that search would
# quietly find the host's x86-64 headers instead.
#
# native/CMakeLists.txt also runs its test executables as POST_BUILD steps.
# Once CMAKE_SYSTEM_NAME is set CMake counts the build as a cross build and
# stops resolving those commands to the built executable's path unless an
# emulator is named -- the build then fails with "cordial_pipewire_backend_test:
# No such file or directory". So the aarch64 tests run under qemu-user against
# the same sysroot, which also means they actually execute aarch64 code, and
# the x86_64 mode leaves CMAKE_SYSTEM_NAME unset since it is not a cross build.
toolchain="$CARGO_TARGET_DIR/toolchain-$ARCH.cmake"
mkdir -p "$CARGO_TARGET_DIR"
if [ "$ARCH" = aarch64 ]; then
    cross="set(CMAKE_SYSTEM_NAME Linux)
set(CMAKE_SYSTEM_PROCESSOR aarch64)
set(CMAKE_CROSSCOMPILING_EMULATOR qemu-aarch64;-L;$sysroot)"
else
    cross=""
fi
cat > "$toolchain" <<EOF
$cross
set(CMAKE_SYSROOT $sysroot)
set(CMAKE_C_COMPILER clang)
set(CMAKE_CXX_COMPILER clang++)
set(CMAKE_C_COMPILER_TARGET $triple)
set(CMAKE_CXX_COMPILER_TARGET $triple)
set(CMAKE_EXE_LINKER_FLAGS_INIT -fuse-ld=lld)
set(CMAKE_SHARED_LINKER_FLAGS_INIT -fuse-ld=lld)
set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_PACKAGE ONLY)
EOF

t=${rust_target//-/_}
T=$(echo "$t" | tr '[:lower:]' '[:upper:]')
flags="--target=$triple --sysroot=$sysroot"
export "CC_$t=clang" "CXX_$t=clang++" "AR_$t=$(command -v llvm-ar || echo /usr/lib/llvm-21/bin/llvm-ar)"
export "CFLAGS_$t=$flags" "CXXFLAGS_$t=$flags"
export "CMAKE_TOOLCHAIN_FILE_$t=$toolchain"
export "CARGO_TARGET_${T}_LINKER=clang"
export "CARGO_TARGET_${T}_RUSTFLAGS=-C link-arg=--target=$triple -C link-arg=--sysroot=$sysroot -C link-arg=-fuse-ld=lld"
# pkg-config is only ever asked about the target here: build scripts run on
# the host but link nothing from pkg-config themselves.
export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_SYSROOT_DIR="$sysroot"
export PKG_CONFIG_LIBDIR="$sysroot/usr/lib/$triple/pkgconfig:$sysroot/usr/share/pkgconfig:$sysroot/usr/lib/pkgconfig"
unset PKG_CONFIG_PATH

# patches/0005-0008 are applied by the build scripts themselves, to a copy
# under the target directory (patches/apply.rs), the same as for a plain
# `cargo build`; this script used to apply them to the submodules in place.

cd "$repo"
# `cargo <subcommand> ...` runs that instead of building the client, so tests
# and clippy see exactly the compilers, sysroot and target dir the build does
# rather than a hand-copied variant of this script.
if [ "${1:-}" = cargo ]; then
    shift
    sub=$1
    shift
    exec cargo "$sub" --target "$rust_target" "$@"
fi
# BINS names the binaries to build, `cordial-run` alone by default. The
# launcher is `cordial-shell`, and building it here is the only way a host
# without GTK 4 development packages can run the window that starts a VR
# launch: BINS="cordial-run cordial-shell".
bins=${BINS:-cordial-run}
cargo build --release $(printf -- '--bin %s ' $bins) --features cordial-runtime/vr --target "$rust_target" "$@"
for bin in $bins; do echo "built $CARGO_TARGET_DIR/$rust_target/release/$bin"; done
