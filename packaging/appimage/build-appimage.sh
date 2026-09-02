#!/usr/bin/env bash
# Build a distro-agnostic AppImage for Cordial.
#
# This is the packaging format with the most work still to prove out, and
# that is worth saying before the recipe: an AppImage's whole job is to carry
# its own copies of libraries a host might not have at the right version, and
# Cordial links four things a bare `ldd`-following bundler does not fully
# reach -- GTK4, libadwaita and WebKitGTK are ordinary DT_NEEDED dependencies
# that linuxdeploy's ELF walk does find, but WebKitGTK's own helper
# processes (WebKitWebProcess, WebKitNetworkProcess, WebKitGPUProcess), its
# injected bundle, and the bwrap and xdg-dbus-proxy it shells out to for its
# own sandbox are reached through absolute paths baked into
# libwebkitgtk-6.0.so, and GSettings schemas are found by path rather than by
# symbol. All of those are bundled below by hand rather than by the bundler.
# Making the baked-in paths *resolve* on a host that has never installed
# WebKitGTK is AppRun's job, and it is a mount namespace rather than an
# environment variable, because WebKitGTK 2.52 offers no environment variable
# for them -- AppRun carries the measurement and the offsets.
#
# This paragraph used to end "nobody has launched one yet". Somebody has, on
# 2026-08-27: the shell starts and draws on Fedora 44 (Bluefin, GNOME,
# Wayland), first-run window, profile row, Roblox button. The schema handling
# below survived that; WEBKIT_EXEC_PATH did not.
#
# **And a user hit exactly that on 2026-09-02**, from an AppImage mounted at
# /tmp/.mount_Cordia...: `Failed to spawn child process
# "/usr/libexec/webkitgtk-6.0/WebKitNetworkProcess" (No such file or
# directory)`. Installing WebKitGTK on their host did not help, and could not
# have: only Fedora and its derivatives put the helpers under /usr/libexec.
# Debian and Ubuntu ship them at /usr/lib/x86_64-linux-gnu/webkitgtk-6.0 and
# Arch at /usr/lib/webkitgtk-6.0, so the package installs and the path the
# bundled Fedora library asks for is still empty.
#
# AppRun now makes those paths exist in a mount namespace of its own, and this
# script bundles what goes in them. Measured on 2026-09-02 against an AppImage
# built from this recipe, on a host standing in for one that has never
# installed WebKitGTK -- /usr/libexec an empty tmpfs, /usr/bin a copy of itself
# with bwrap and xdg-dbus-proxy removed: with the wrap, WebKitNetworkProcess
# and WebKitWebProcess both ran out of the image, loaded the image's own
# libwebkitgtk and injected bundle, the sandbox engaged on the planted bwrap,
# and a page finished loading; with CORDIAL_APPIMAGE_NO_WRAP=1, the same image
# on the same host produced the reported error verbatim. AppRun carries the
# full reading.
#
# Two caveats on that. The binaries inside the tested image came from the
# installed cordial rpm rather than from the cargo build below, because the
# machine it was measured on has no webkitgtk6.0-devel and cannot compile the
# webview feature; and the probe was a WebKitWebView, not cordial-shell's own
# sign-in view. Still unexercised: any machine that is not this one, and any
# distro that is not Fedora. Say which of those states a report is about.
#
# Built inside registry.fedoraproject.org/fedora:44 -- the one environment
# this repository has proven builds gtk4 4.22/libadwaita 1.9 correctly
# (test.yml exists because Fedora 43 and Ubuntu 24.04 are both older and fail
# three *-sys build scripts). The AppImage bundles what that container has so
# the result runs on hosts with much older, or no, GTK4 at all.
#
# Usage:
#     packaging/appimage/build-appimage.sh [--outdir DIR]
#
# Needs: cargo, clang/clang++, cmake, pkg-config, the GTK4/libadwaita/
# WebKitGTK development headers, rsvg-convert (for the AppDir's PNG icon --
# AppImage integration tooling looks for one at the AppDir root even though
# Cordial's own icon is scalable SVG everywhere else), patchelf (the host's,
# not linuxdeploy's -- see the NO_STRIP/PATCHELF block below for what its
# bundled 0.15 does to a library with a .relr.dyn section), and network access
# to fetch linuxdeploy and appimagetool on first use.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=$(git -C "$here" rev-parse --show-toplevel)
outdir="$repo/dist/appimage"
while [ $# -gt 0 ]; do
    case "$1" in
        --outdir) outdir=$2; shift 2 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

cd "$repo"
eval "$(packaging/version.sh)"
echo "==> building cordial ${CORDIAL_DESCRIBE}"

need() {
    command -v "$1" >/dev/null 2>&1 || {
        echo "error: $1 is not installed" >&2
        exit 1
    }
}
for tool in cargo clang rsvg-convert readelf glib-compile-schemas patchelf; do
    need "$tool"
done

# Where cargo will actually put the binaries. This used to be spelled `target/`
# at every use, which is right only when nothing exported CARGO_TARGET_DIR --
# and CLAUDE.md tells every agent working in a worktree to export one, because
# two builds sharing a target/ once produced two rlibs neither of which held a
# symbol plainly in the source. With one exported, cargo wrote into it and the
# very next line here read `target/release/cordial-run`, which did not exist.
target_dir="${CARGO_TARGET_DIR:-$repo/target}"

tools_dir="${CORDIAL_APPIMAGE_TOOLS_DIR:-$target_dir/appimage-tools}"
mkdir -p "$tools_dir"

fetch_pinned() {
    # A fixed release and its own sha256, computed by hand against the file
    # this pins and checked in here rather than trusted from a remote
    # checksums file, because neither AppImage/appimagetool nor
    # linuxdeploy/linuxdeploy publishes one on their release pages -- GitHub's
    # own asset listing carries a size and a download count, nothing more.
    # Bump the version and the sum together if either tool is ever updated.
    local url=$1 sha256=$2 dest=$3
    if [ -f "$dest" ] && echo "${sha256}  ${dest}" | sha256sum --check --status; then
        return 0
    fi
    curl --fail --location --retry 3 --output "$dest" "$url"
    echo "${sha256}  ${dest}" | sha256sum --check --status || {
        echo "error: $dest does not match the pinned checksum" >&2
        exit 1
    }
    chmod +x "$dest"
}

fetch_pinned \
    https://github.com/linuxdeploy/linuxdeploy/releases/download/1-alpha-20251107-1/linuxdeploy-x86_64.AppImage \
    c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d \
    "$tools_dir/linuxdeploy-x86_64.AppImage"
fetch_pinned \
    https://github.com/AppImage/appimagetool/releases/download/1.9.0/appimagetool-x86_64.AppImage \
    46fdd785094c7f6e545b61afcfb0f3d98d8eab243f644b4b17698c01d06083d1 \
    "$tools_dir/appimagetool-x86_64.AppImage"

# Both tools are themselves AppImages, and this normally needs FUSE to mount.
# A container has none, so this extracts and runs instead -- the same
# variable mocktail's own packages.yml sets around the equivalent step.
export APPIMAGE_EXTRACT_AND_RUN=1

# linuxdeploy carries its own binutils from 2020 and its own patchelf from
# 2022, and Fedora 44 emits a `.relr.dyn` (SHT_RELR, type 0x13) section in
# every system library. Neither bundled tool knows that section type, and both
# fail in a way that reads as something else entirely:
#
#   strip 2.35    -- "unknown type [0x13] section `.relr.dyn'" on 167 of the
#                    bundled libraries, and linuxdeploy exits 1 on the first.
#   patchelf 0.15 -- succeeds, and relocates .init to the end of the file
#                    without updating DT_INIT. 161 of 165 bundled .so files
#                    came out with DT_INIT still naming the old address; the
#                    loader called libcbor's at base+0x2cc, which is now the
#                    ELF header, and the AppImage took SIGSEGV in call_init
#                    before main. Nothing printed. It looked like a crash in
#                    Cordial.
#
# NO_STRIP costs nothing for the bundled libraries -- Fedora's are already
# stripped and their debug info lives in separate debuginfo packages. **It cost
# a great deal for ours, and this comment used to say otherwise.** Cordial's own
# two binaries carry full DWARF by `[profile.release]`, so leaving linuxdeploy's
# stripping off left roughly 350 MB of symbol tables inside the AppImage --
# which is why it was 176 MB against the rpm's 6. They are stripped above,
# before linuxdeploy runs, which is the right place: the reason to disable its
# stripping is what its 2020 binutils does to a `.relr.dyn` section, and that
# reason has nothing to do with our output. $PATCHELF is
# linuxdeploy's own override; the host's patchelf 0.18 leaves .init at 0x2cc
# and DT_INIT agreeing with it, checked on the same libcbor both ways.
export NO_STRIP=1
export PATCHELF="${PATCHELF:-$(command -v patchelf)}"

export CC=clang CXX=clang++
# `CORDIAL_GIT_SHA`, not a version. `Cargo.toml` is the version now and a
# packager may not override it -- a release job that stamped its own would
# be the second, disagreeing number this scheme exists to remove. The build
# happens outside a git checkout here, so the commit has to be passed in.
export CORDIAL_GIT_SHA="$CORDIAL_SHORTHASH"

# Both crates' `webview` features, never one alone -- see the identical
# comment in packaging/rpm/cordial.spec's %build and packaging/deb/build-deb.sh
# for the shape of the bug that taught this project to say so at every
# callsite: with only one crate's feature on, the linker collects
# webview::open silently and the binary carries no WebKitGTK, with no error
# anywhere in the build.
cargo build --release --locked \
    --features cordial-shell/webview,cordial-runtime/webview

readelf -d "$target_dir/release/cordial-run" | grep -qi webkit || {
    echo "cordial-run linked no WebKitGTK; the webview features did not take" >&2
    exit 1
}

appdir="$target_dir/appimage/AppDir"
rm -rf "$appdir"
mkdir -p "$appdir"

install -Dm755 "$target_dir/release/cordial-shell" "$appdir/usr/bin/cordial-shell"
# **`cordial` is the command; `cordial-shell` is the file.** Asked for on
# 2026-08-28: nobody wants to type the second word, and every other launcher on
# a desktop answers to its own name. A symlink rather than a rename so that
# anything already invoking `cordial-shell` -- a .desktop file somebody edited,
# a script, a bug report -- keeps working, and so the two binaries stay
# obviously related in `ls /usr/bin`. `cordial-run` deliberately gets no alias:
# it is the loader the shell launches and is not what anyone should run by hand.
ln -sf cordial-shell "$appdir/usr/bin/cordial"
# First-party plugins, read-only beside the binary.
# Until the native packages existed nothing installed these anywhere, so the settings window listed no built-in plugins for anybody -- including Flatpak users, whose /app/share/cordial/plugins the code has looked in from the start and which has never existed.
for plugin in plugins/*/; do
    id=$(basename "$plugin")
    [ -f "$plugin/plugin.json" ] || continue
    install -Dm644 "$plugin/plugin.json" "$appdir/usr/share/cordial/plugins/$id/plugin.json"
    install -Dm644 "$plugin/main.ts"     "$appdir/usr/share/cordial/plugins/$id/main.ts"
done

install -Dm755 "$target_dir/release/cordial-run"   "$appdir/usr/bin/cordial-run"

# **Strip our own two binaries, which are almost entirely debug info.**
# `[profile.release]` in Cargo.toml sets `debug = true`, deliberately -- AGENTS.md
# leans on lldb and gdb against a running client and says so at length, and a
# runtime you cannot get a backtrace out of is not worth the disk it saves. But
# that is an argument about the build on a developer's machine, not about what a
# user downloads: `cordial-run` is 207.4 MB unstripped and 15.7 MB with
# `--strip-debug`, measured here, and `cordial-shell` is another 175.7 MB.
#
# Stripping at packaging time keeps both: full DWARF where somebody is debugging,
# and a package that is not thirteen times larger than it needs to be. rpmbuild
# and makepkg already do this by themselves, which is the whole reason the rpm
# and the Arch package were a tenth the size of the others.
#
# Done here, with the host's strip, before linuxdeploy sees the AppDir. NO_STRIP
# below turns off linuxdeploy's own stripping because its bundled binutils is
# from 2020 and mangles a `.relr.dyn` section -- but that is a reason to keep it
# away from Fedora's libraries, not a reason to ship ours unstripped.
strip --strip-debug "$appdir/usr/bin/cordial-shell" "$appdir/usr/bin/cordial-run"

install -Dm644 packaging/io.github.luohoa97.Cordial.desktop \
    "$appdir/usr/share/applications/io.github.luohoa97.Cordial.desktop"
install -Dm644 packaging/io.github.luohoa97.Cordial.metainfo.xml \
    "$appdir/usr/share/metainfo/io.github.luohoa97.Cordial.metainfo.xml"
install -Dm644 packaging/icons/hicolor/scalable/apps/io.github.luohoa97.Cordial.svg \
    "$appdir/usr/share/icons/hicolor/scalable/apps/io.github.luohoa97.Cordial.svg"
install -Dm644 packaging/icons/hicolor/scalable/apps/io.github.luohoa97.Cordial.Frostbite.svg \
    "$appdir/usr/share/icons/hicolor/scalable/apps/io.github.luohoa97.Cordial.Frostbite.svg"

licdir="$appdir/usr/share/licenses/cordial"
install -Dm644 LICENSE "$licdir/LICENSE"
install -Dm644 NOTICE "$licdir/NOTICE"
install -Dm644 THIRD-PARTY-NOTICES.md "$licdir/THIRD-PARTY-NOTICES.md"
install -Dm644 third_party/libbadcpu/LICENSE.upstream "$licdir/libbadcpu-MIT.txt"
install -Dm644 third_party/mcpelauncher-linker/LICENSE "$licdir/mcpelauncher-linker-MIT.txt"
install -Dm644 third_party/mcpelauncher-linker/core/NOTICE "$licdir/aosp-NOTICE.txt"
install -Dm644 third_party/libjnivm/LICENSE "$licdir/libjnivm-MIT.txt"
install -Dm644 third_party/mocktail-webview/LICENSE "$licdir/mocktail-webview-Apache-2.0.txt"
# bwrap and xdg-dbus-proxy are whole programs this AppImage now redistributes,
# not libraries swept up by a dependency walk, so their licences travel with
# them. Both are LGPL and both ship a COPYING under /usr/share/licenses.
for pkg in bubblewrap:bwrap-LGPL.txt xdg-dbus-proxy:xdg-dbus-proxy-LGPL.txt; do
    src=/usr/share/licenses/${pkg%%:*}/COPYING
    [ -f "$src" ] && install -Dm644 "$src" "$licdir/${pkg##*:}" || true
done

install -Dm755 packaging/appimage/AppRun "$appdir/AppRun"

# AppImage integration tooling (and appimagetool's own validation) wants a
# desktop file and an icon at the AppDir root, not only under usr/share/. A
# copy rather than a symlink, because appimagetool refuses to package a
# symlink pointing outside the tree it is squashing.
cp "$appdir/usr/share/applications/io.github.luohoa97.Cordial.desktop" \
    "$appdir/io.github.luohoa97.Cordial.desktop"
# Rasterised because AppImage's own integration (and thumbnailers that read
# AppImages without extracting them) commonly assume a PNG at the root even
# where the desktop's Icon= key resolves an SVG everywhere else -- 256x256
# matches the largest size Cordial's own icon theme directory would carry had
# one been rendered, and is large enough not to look soft in a file manager.
rsvg-convert --width 256 --height 256 \
    packaging/icons/hicolor/scalable/apps/io.github.luohoa97.Cordial.svg \
    -o "$appdir/io.github.luohoa97.Cordial.png"

echo "==> bundling shared libraries with linuxdeploy"
# Plain linuxdeploy, deliberately with no GTK plugin. linuxdeploy-plugin-gtk
# targets GTK3's module and schema layout; this workspace is GTK4 and
# libadwaita, which the plugin does not know how to bundle, and using it
# for the wrong toolkit version risks bundling GTK3 pieces alongside GTK4
# ones rather than helping. The library discovery linuxdeploy does on its
# own -- walking each --executable's ELF dependencies and copying what it
# finds into usr/lib, then rewriting rpaths -- is toolkit-agnostic and is
# the part actually needed here; GSettings schemas and the WebKitGTK helper
# binaries, which that walk cannot see because neither is a DT_NEEDED entry,
# are handled by hand below instead.
webkit_libexec=$(rpm -ql webkitgtk6.0 2>/dev/null | grep -m1 '/libexec/webkitgtk-6.0$' || true)
webkit_bundle=$(rpm -ql webkitgtk6.0 2>/dev/null | grep -m1 '/webkitgtk-6.0/injected-bundle$' || true)
deploy_args=(--executable "$appdir/usr/bin/cordial-shell" --executable "$appdir/usr/bin/cordial-run")
if [ -n "$webkit_libexec" ] && [ -d "$webkit_libexec" ]; then
    # Every helper binary passed as its own --executable, not just copied,
    # so linuxdeploy's dependency walk covers *their* DT_NEEDED entries too
    # -- a WebProcess is a separate executable with its own library needs,
    # not merely a data file sitting next to cordial-run.
    while IFS= read -r -d '' helper; do
        deploy_args+=(--executable "$helper")
    done < <(find "$webkit_libexec" -maxdepth 1 -type f -executable -print0)
else
    # Never make a stub lie: better a loud build failure than an AppImage
    # that silently ships a web view with no process to run it in, which is
    # exactly the "webview doesnt work" shape this project has already hit
    # once from the Flatpak missing a Cargo feature rather than a binary.
    echo "error: could not find webkitgtk-6.0's libexec directory (WebKitWebProcess, WebKitNetworkProcess)" >&2
    echo "  the AppImage's web view would have no process to run in" >&2
    exit 1
fi

# The injected bundle: a .so the WebProcess dlopens, from a second absolute
# path baked into libwebkitgtk-6.0.so (/usr/lib64/webkitgtk-6.0/injected-bundle
# on Fedora, /usr/lib/x86_64-linux-gnu/webkitgtk-6.0/injected-bundle on Debian).
# --library rather than --executable because it is dlopened, not exec'd, and
# linuxdeploy's walk still needs to see it or its own dependencies go
# unbundled.
if [ -n "$webkit_bundle" ] && [ -d "$webkit_bundle" ]; then
    while IFS= read -r -d '' so; do
        deploy_args+=(--library "$so")
    done < <(find "$webkit_bundle" -maxdepth 1 -name '*.so' -print0)
else
    echo "error: could not find webkitgtk-6.0's injected-bundle directory" >&2
    echo "  the AppImage's web processes would have no bundle to load" >&2
    exit 1
fi

# bwrap and xdg-dbus-proxy. WebKitGTK 6.0 has no API to turn its sandbox off
# -- the 4.x webkit_web_context_set_sandbox_enabled is gone -- and the sandbox
# is not in-process: the library execs /usr/bin/bwrap, and /usr/bin/xdg-dbus-proxy
# to filter the session bus, both by absolute path, both baked in beside the
# helper path above. The webkitgtk6.0 rpm Requires both by name, so a container
# with the WebKitGTK devel headers has them by construction; a *user's* machine
# that never installed WebKitGTK has neither, which is the whole case this
# AppImage is for. Passed as --executable so their own DT_NEEDED entries
# (libcap, libselinux) are bundled -- a planted binary that cannot start is
# the same gap one level down.
for tool in bwrap xdg-dbus-proxy; do
    tool_path=$(command -v "$tool" 2>/dev/null || true)
    if [ -z "$tool_path" ]; then
        echo "error: $tool is not installed" >&2
        echo "  WebKitGTK execs it by absolute path for its own sandbox, and the" >&2
        echo "  AppImage would carry no copy to plant on a host that lacks one" >&2
        exit 1
    fi
    deploy_args+=(--executable "$tool_path")
done

"$tools_dir/linuxdeploy-x86_64.AppImage" \
    --appdir "$appdir" \
    "${deploy_args[@]}" \
    --desktop-file "$appdir/io.github.luohoa97.Cordial.desktop" \
    --icon-file "$appdir/io.github.luohoa97.Cordial.png"

echo "==> laying out what WebKitGTK reaches by absolute path"
# linuxdeploy was just given each helper as an --executable so their own
# library dependencies land in usr/lib, but linuxdeploy places binaries named
# as --executable next to usr/bin, not back where WebKitGTK's ProcessLauncher
# expects to find them. The layout below is not decoration: AppRun binds each
# of these directories over the path baked into libwebkitgtk-6.0.so, in a
# mount namespace of its own, and it binds them *by these names*. Change one
# here and change it there.
#
#   usr/libexec/webkitgtk-6.0          -> /usr/libexec/webkitgtk-6.0
#   usr/lib/webkitgtk-6.0/injected-bundle
#                                      -> /usr/lib64/webkitgtk-6.0, or, where
#                                         that cannot be bound, reached by
#                                         WEBKIT_INJECTED_BUNDLE_PATH
#   usr/bin/bwrap, usr/bin/xdg-dbus-proxy
#                                      -> /usr/bin/..., planted only on a host
#                                         that has none
#
# The environment variable that would have made all of this unnecessary,
# WEBKIT_EXEC_PATH, does not exist in the shipped library at all -- zero
# occurrences against thirty-odd other WEBKIT_* names -- and exporting it
# anyway, measured 2026-08-27, left MiniBrowser spawning the *host's*
# WebKitWebProcess. AppRun carries the full measurement and the byte offsets
# of each baked-in path.
install -d "$appdir/usr/libexec/webkitgtk-6.0"
find "$webkit_libexec" -maxdepth 1 -type f -executable -exec \
    install -m755 {} "$appdir/usr/libexec/webkitgtk-6.0/" \;
install -d "$appdir/usr/lib/webkitgtk-6.0/injected-bundle"
find "$webkit_bundle" -maxdepth 1 -name '*.so' -exec \
    install -m755 {} "$appdir/usr/lib/webkitgtk-6.0/injected-bundle/" \;
# linuxdeploy puts --executable binaries in usr/bin already, but say so
# explicitly rather than depending on that: AppRun tests for these two paths
# by name and silently skips planting them if they are absent, which is
# exactly the kind of quiet gap this file exists to avoid.
for tool in bwrap xdg-dbus-proxy; do
    [ -x "$appdir/usr/bin/$tool" ] || install -m755 "$(command -v "$tool")" "$appdir/usr/bin/$tool"
done
# WebKitGTK also reads its own injected bundle and sandbox profile from
# beside the libexec directory in some layouts; copied best-effort rather
# than gated on, since an absent one is a narrower loss (likely the GPU
# process sandbox) than an absent helper binary is.
webkit_share=$(rpm -ql webkitgtk6.0 2>/dev/null | grep -m1 '/share/webkitgtk-6.0$' || true)
if [ -n "$webkit_share" ] && [ -d "$webkit_share" ]; then
    install -d "$appdir/usr/share/webkitgtk-6.0"
    cp -a "$webkit_share/." "$appdir/usr/share/webkitgtk-6.0/"
fi

echo "==> compiling GSettings schemas into the AppDir"
# Looked up by GIO through GSETTINGS_SCHEMA_DIR at runtime (see AppRun), not
# discoverable from any binary's DT_NEEDED entries, so linuxdeploy's walk
# above never touches this. Best-effort: GTK4/libadwaita read a handful of
# schemas for things like colour-scheme preference, and their absence is a
# fallback-to-default rather than a crash, which is a materially smaller risk
# than the WebKitGTK helper binaries above -- hence no hard failure here.
schemas_src=/usr/share/glib-2.0/schemas
if [ -d "$schemas_src" ]; then
    install -d "$appdir/usr/share/glib-2.0/schemas"
    cp "$schemas_src"/*.xml "$appdir/usr/share/glib-2.0/schemas/" 2>/dev/null || true
    glib-compile-schemas "$appdir/usr/share/glib-2.0/schemas"
else
    echo "warning: $schemas_src not found; the AppImage ships no compiled GSettings schemas" >&2
fi

echo "==> appimagetool"
outfile="$outdir/Cordial-${CORDIAL_DESCRIBE}-x86_64.AppImage"
mkdir -p "$outdir"
ARCH=x86_64 "$tools_dir/appimagetool-x86_64.AppImage" "$appdir" "$outfile"

chmod +x "$outfile"
ls -lh "$outfile"
echo "built: $outfile"
echo
echo "The shell starts and draws: launched on Fedora 44 (Bluefin, GNOME,"
echo "Wayland) on 2026-08-27, first-run window titled with CORDIAL_DESCRIBE,"
echo "profile row and Roblox button, as a wl_surface with the right app id."
echo
echo "The web view now travels. WebKitGTK 2.52 ignores WEBKIT_EXEC_PATH and"
echo "reaches its three helper processes, its injected bundle, bwrap and"
echo "xdg-dbus-proxy through absolute paths baked into libwebkitgtk-6.0.so;"
echo "all five are bundled here and AppRun binds them over those paths in a"
echo "mount namespace of its own. Measured 2026-09-02 on a host standing in"
echo "for one with no WebKitGTK, no bwrap and no xdg-dbus-proxy: both helpers"
echo "ran out of the image, the sandbox engaged, a page finished loading --"
echo "and the same image with CORDIAL_APPIMAGE_NO_WRAP=1 gave the reported"
echo "spawn error verbatim. Not yet measured through cordial-shell's own"
echo "sign-in view, or on any distribution other than Fedora."
echo
echo "If bwrap or unprivileged overlayfs is unavailable, AppRun says so on"
echo "stderr and carries on unwrapped; the web view then needs the host to"
echo "have WebKitGTK 6.0 at Fedora's /usr/libexec path, which Debian, Ubuntu"
echo "and Arch do not use."
