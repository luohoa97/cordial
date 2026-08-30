# Cordial, for Fedora Copr.
#
# Cordial vendors no Roblox code. Roblox publishes no Android binary of its own
# -- its endpoint answers `supportsAndroidBinaries: false` -- so the engine
# comes from a third-party mirror, signature-checked against Roblox's own
# certificate, or from the copy Sober unpacked, or from an APK the user points
# it at. That is said in the summary, in %%description and again in the
# post-install note, because a package registry is where a false promise costs
# most -- and this file made exactly that promise in the wrong direction until
# 2026-08-28, telling users Cordial could not fetch a build when it has since
# v0.9.0.
#
# Build an SRPM with packaging/rpm/make-srpm.sh, which is what fills in the
# %%global lines directly below from `git describe --tags`. Do not edit them by
# hand; a spec whose snapinfo disagrees with its tarball is a build that fails
# in %%prep for a reason nobody can read.

# **Clang is not a preference and must be stated rather than hoped for.**
# AOSP bionic uses C11 _Atomic inside C++ headers, which GCC rejects with 144
# errors, and native/CMakeLists.txt refuses a non-Clang compiler outright. This
# macro is what makes redhat-rpm-config hand CC/CXX and the flag set to clang;
# without it the failure lands deep inside a *-sys build script that names
# neither the tool nor the cause.
%global toolchain clang

# Filled in by make-srpm.sh from `git describe --tags`. The defaults below
# describe the commit this file was last verified against, so that a plain
# `rpmbuild` against a matching tarball still works.
%global snapinfo 108.20260822git9d9c980
%global commit   9d9c9800000000000000000000000000000000000

# The exact `git describe` string, passed to the build so the title bar agrees
# with `rpm -q`. Without it the tarball has no .git, crates/cordial-shell/build.rs
# falls back to the bare Cargo version, and the client calls itself "Cordial
# 0.6.0" while the package it came from is 0.6.0-1.108.20260822git9d9c980. It
# also stops git walking up out of an unpacked tarball and stamping the tag of
# whatever unrelated repository happens to sit above it, which is the same bug
# wearing a convincing number.
%global describe 0.6.0-108-g9d9c980

%global archivename %{name}-%{version}-%{snapinfo}

Name:           cordial
Version:        0.6.0
# The distance from the tag leads, so snapshots sort: 1.108.<date>git<hash>
# then 1.112.<date>git<hash>. rpmvercmp compares 108 and 112 numerically.
Release:        1.%{snapinfo}%{?dist}
Summary:        Run Roblox natively on Linux -- you supply the Roblox build, none is shipped

# The workspace is GPL-3.0-or-later; the vendored subtrees that end up in the
# binary carry their own notices, installed alongside it.
License:        GPL-3.0-or-later AND MIT AND Apache-2.0
URL:            https://github.com/luohoa97/cordial

# Both produced by packaging/rpm/make-srpm.sh. Source0 carries the working tree
# *including* third_party/mcpelauncher-linker (and its own bionic and core
# submodules) and third_party/libjnivm, because the native subtree does not
# build without them and %%prep has no network. Source1 is `cargo vendor`, for
# the same reason -- Copr builds may run with networking off, and a build that
# only works when it happens to be on is not reproducible.
Source0:        %{archivename}.tar.zst
Source1:        %{archivename}-vendor.tar.zst

# x86-64 only, and this is architectural rather than an untested-elsewhere
# caveat: the engine is Roblox's Android **x86-64** build, executed natively
# with no CPU translation. See docs/multiarch.md.
ExclusiveArch:  x86_64

BuildRequires:  cargo-rpm-macros >= 24
BuildRequires:  cargo
BuildRequires:  rust
BuildRequires:  clang
BuildRequires:  cmake
BuildRequires:  make
BuildRequires:  pkgconf-pkg-config
BuildRequires:  gtk4-devel
BuildRequires:  libadwaita-devel
# The in-experience web window. Built in below with both crates' `webview`
# feature; %%check proves it actually linked.
BuildRequires:  webkitgtk6.0-devel
# Headers only. native/CMakeLists.txt compiles the real OpenSL ES backend when
# it finds them and the previous link-only stub when it does not -- so leaving
# this out is a silent loss of audio rather than a build failure.
BuildRequires:  pipewire-devel
# The same rule, for the second host backend (ADR-023). `libpulse.so.0` is
# dlopen'd and never linked, so this is headers only and adds no runtime
# dependency -- but without it `native/pulse_backend.cpp` compiles its "this
# backend is unavailable" arm, and a user who sets CORDIAL_AUDIO_HOST=pulse is
# told there is no PulseAudio on a machine that has one. That is the same
# silent-loss shape the line above exists to prevent, and this project has
# already lost every sample of audio it ever played to it once.
BuildRequires:  pulseaudio-libs-devel
# And the third (ADR-023). Headers only; `libasound.so.2` is dlopen'd too.
BuildRequires:  alsa-lib-devel
# `-lz` on the native link line.
BuildRequires:  zlib-ng-compat-devel
BuildRequires:  desktop-file-utils
BuildRequires:  libappstream-glib

# Everything below is dlopen'd rather than linked, so rpm's automatic
# dependency generator cannot see any of it and dropping one produces a missing
# feature rather than a missing symbol at load.
#
# **Written as sonames rather than package names, and that is not cosmetic.**
# A soname is what dlopen actually asks for, so the dependency says the true
# thing; rpmlint's `explicit-lib-dependency` goes quiet as a side effect rather
# than as the point. The reason it matters was learned on the Arch side of this
# same work on the same day: `gcc-libs` there has been split, and a package
# naming it rather than the library was right only by inheritance. A soname
# survives a rename; a package name does not.
Requires:       libvulkan.so.1()(64bit)
Requires:       libwayland-client.so.0()(64bit)
Requires:       libwayland-egl.so.1()(64bit)
Requires:       libxkbcommon.so.0()(64bit)
Requires:       libEGL.so.1()(64bit)
Requires:       libGLESv2.so.2()(64bit)
# native/pipewire_backend.cpp dlopens libpipewire-0.3.so.0. Without it there is
# no sound and nothing says so.
Requires:       libpipewire-0.3.so.0()(64bit)
Requires:       hicolor-icon-theme

# Roblox's own build, unpacked by Sober, is the first place Cordial looks.
# A suggestion rather than a dependency, because a user-supplied APK does just
# as well and neither one comes from this package. Sober is a Flatpak and has
# no RPM, so this cannot be a Recommends that resolves.
%global sober_hint ~/.var/app/org.vinegarhq.Sober/data/sober/packages/x86_64/com.roblox.client/

%description
Cordial loads Roblox's official Android x86-64 libroblox.so natively on Linux:
a ported AOSP bionic linker, a bionic/glibc shim, libjnivm in place of Android's
ART, and a framework layer that answers the calls the client makes into the
platform. There is no emulation and no CPU translation.

CORDIAL SHIPS NO ROBLOX BUILD. On first run it offers to fetch one and installs
it only if Roblox's own signing certificate signed it. Roblox publishes no
Android binary of its own -- its own endpoint answers supportsAndroidBinaries:
false -- so the build comes from a third-party mirror, from a copy Sober has
already unpacked, or from an APK you supply yourself. A local copy is preferred
when there is one, and gets the same signature check either way.

Run it by typing `cordial`.

Cordial is early. You can sign in, load an experience, play with a keyboard and
mouse, type into text fields and hear sound. Voice chat does not work, and on
roughly one launch in three a signed-in client reaches the home screen and
freezes; reopening usually works. The project's README says which claims were
measured and how.

%prep
%autosetup -n %{name}-%{version} -N -a 1
# -v rather than the system registry: this workspace pins 212 crates by the
# sha256 already in Cargo.lock, and cargo_prep keeps Cargo.lock in place when
# vendored sources are used. It also creates target/rpm and symlinks
# target/release at it, which is why the paths below say target/release.
%cargo_prep -v vendor

%build
# %%global toolchain clang above already puts clang in CC/CXX for the rpm build
# environment; this repeats it because it is the single requirement most likely
# to be lost when somebody adapts this spec, and the failure it prevents lands
# deep inside a *-sys build script naming neither the tool nor the cause.
export CC=clang CXX=clang++

# See the %%describe comment at the top: the tarball has no .git, so the stamp
# has to be handed in or the client misreports its own version.
# `CORDIAL_GIT_SHA`, not a version: Cargo.toml is the version and a packager
# may not override it. See crates/cordial-shell/src/version.rs.
# Derived from %%{describe}, which make-srpm.sh already substitutes, rather
# than a %%{shorthash} macro -- there is no such macro, and rpm would have
# shipped the literal text. `0.6.0-108-g9d9c980` -> `9d9c980`.
export CORDIAL_GIT_SHA=$(printf %s %{describe} | sed "s/.*-g//")

# Both crates' `webview` features, and one without the other is the trap. The
# shell holds the WebKit window and cordial-runtime holds the presenter that
# calls it, so enabling only the shell's leaves the caller cfg'd out, the
# linker collects webview::open, and the binary links no WebKit at all. That
# shipped once in the Flatpak and was reported as "webview doesnt work in
# cordial flatpak". %%check proves it linked rather than trusting this line.
%cargo_build -f cordial-shell/webview,cordial-runtime/webview

%install
# Not %%cargo_install: this is a virtual workspace with no [package] section,
# and cargo2rpm's is-bin probe has nothing to read. Two binaries by hand, which
# is what the Flatpak manifest does for the same reason.
#
# Both of them, side by side. launch.rs looks for the loader as the sibling of
# current_exe and nowhere else -- there is no baked-in path and nothing to
# configure -- so a shell installed without cordial-run beside it is a launcher
# whose Launch button cannot find anything to launch.
install -Dpm 0755 target/release/cordial-shell %{buildroot}%{_bindir}/cordial-shell
# **`cordial` is the command; `cordial-shell` is the file.** Asked for on
# 2026-08-28: nobody wants to type the second word. A symlink rather than a
# rename so anything already invoking `cordial-shell` keeps working.
# `cordial-run` deliberately gets no alias -- it is the loader the shell
# launches and is not what anyone should run by hand. Note the doubled percent
# signs above and below are not needed here, but a bare %%name in a comment is
# expanded by rpm before the script runs, which has broken this file before.
ln -sf cordial-shell %{buildroot}%{_bindir}/cordial
install -Dpm 0755 target/release/cordial-run   %{buildroot}%{_bindir}/cordial-run

# The square icons under packaging/icons/hicolor/. Both of them: Frostbite is the
# twice-a-year name in crates/cordial-shell/src/branding.rs, and a missing one
# is a blank icon in the task switcher on the one day nobody is watching for it.
# A test in that file asserts both exist and are square.
install -Dpm 0644 packaging/icons/hicolor/scalable/apps/io.github.luohoa97.Cordial.svg \
    %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/io.github.luohoa97.Cordial.svg
install -Dpm 0644 packaging/icons/hicolor/scalable/apps/io.github.luohoa97.Cordial.Frostbite.svg \
    %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/io.github.luohoa97.Cordial.Frostbite.svg

# The first-party plugins, read-only beside the binary. `system_plugin_root()`
# derives this path from the running executable, so `%{_datadir}` is what it
# finds -- and until the native packages existed it returned Flatpak's `/app`
# unconditionally, which meant a deb or rpm user's settings window listed no
# built-in plugins at all.
for plugin in plugins/*/; do
    id=$(basename "$plugin")
    [ -f "$plugin/plugin.json" ] || continue
    install -Dpm 0644 "$plugin/plugin.json" %{buildroot}%{_datadir}/cordial/plugins/$id/plugin.json
    install -Dpm 0644 "$plugin/main.ts"     %{buildroot}%{_datadir}/cordial/plugins/$id/main.ts
done
install -Dpm 0644 packaging/io.github.luohoa97.Cordial.desktop \
    %{buildroot}%{_datadir}/applications/io.github.luohoa97.Cordial.desktop
install -Dpm 0644 packaging/io.github.luohoa97.Cordial.metainfo.xml \
    %{buildroot}%{_datadir}/metainfo/io.github.luohoa97.Cordial.metainfo.xml

# MIT requires its notice in "all copies or substantial portions" and
# Apache-2.0 section 4(d) requires NOTICE to travel with derivative works. Both
# apply to a binary package, not only to a source tree.
install -Dpm 0644 third_party/libbadcpu/LICENSE.upstream            libbadcpu-MIT.txt
install -Dpm 0644 third_party/mcpelauncher-linker/LICENSE           mcpelauncher-linker-MIT.txt
install -Dpm 0644 third_party/mcpelauncher-linker/core/NOTICE       aosp-NOTICE.txt
install -Dpm 0644 third_party/libjnivm/LICENSE                      libjnivm-MIT.txt
# Apache-2.0 section 4(d): the NOTICE for mocktail-webview, the basis for
# Cordial's own in-experience web window, has to travel with a binary
# distribution and not only with the source tree -- see NOTICE at the
# repository root. Missing from this spec until the packaging pass that added
# .deb, AppImage and a release Arch package noticed the same gap in the
# Flatpak manifest and packaging/aur/cordial-git/PKGBUILD while giving the new
# formats a licence list to copy; all three are fixed in the same change.
# NOTICE itself needs no install line: %%license below picks it up from the
# build directory where %%autosetup already put it, the same way README.md
# reaches %%doc. The line that used to sit here copied it onto itself --
# `install -Dpm 0644 NOTICE NOTICE` -- and install refuses that with "'NOTICE'
# and 'NOTICE' are the same file", failing %%install and taking the whole RPM
# with it. The neighbours above work because each renames as it copies.
#
# The doubled percents are not decoration. **rpm expands macros inside comments
# too**, so the first version of this comment wrote %%autosetup unescaped, rpm
# replaced it with its definition, and `%%setup -q` landed in the middle of the
# generated %%install script -- where bash read a leading percent as a job spec
# and said "fg: no job control". Every other comment in this file already
# doubles them; this one did not, and it cost a CI round.
install -Dpm 0644 third_party/mocktail-webview/LICENSE              mocktail-webview-Apache-2.0.txt

%check
export CC=clang CXX=clang++

# **A tripwire, not a formality.** With the `webview` feature missing from
# either crate the build still succeeds and the package still installs; the
# only symptom is that account settings silently do nothing. This is the same
# check the Flatpak manifest names, and it is here because the equivalent AUR
# package was caught shipping a feature-less binary by it.
readelf -d %{buildroot}%{_bindir}/cordial-run | grep -qi webkit

desktop-file-validate %{buildroot}%{_datadir}/applications/io.github.luohoa97.Cordial.desktop
appstream-util validate-relax --nonet \
    %{buildroot}%{_datadir}/metainfo/io.github.luohoa97.Cordial.metainfo.xml

# The same feature pair as %%build, and leaving it off is not a tidy-up:
# `cargo test` with a different feature resolution rebuilds cordial-run and
# hardlinks the result over target/release/cordial-run, so %%install would
# stage a binary with no web view in it. Measured, on the AUR package, by the
# readelf line above coming back empty after a green test run.
#
# Three tests are skipped and each is a measurement rather than a convenience:
#
#   secrets::tests::a_session_survives_the_round_trip_through_the_service
#   secrets::tests::a_plaintext_store_is_adopted_and_destroyed
#     Both save an item into whatever org.freedesktop.secrets is on the session
#     bus, read it back and erase it. They guard themselves with `usable()` and
#     skip cleanly in a mock chroot, which has no session bus -- but on a
#     developer's machine a package build would be writing to their login
#     keyring, and under load the service missed the module's five-second
#     deadline and failed the build. Seen once, then three clean runs after.
#
#   deep_link::tests::gio_reshapes_a_roblox_link_and_is_therefore_not_where_the_string_comes_from
#     deep_link.rs calls this a tripwire on GIO's URI reshaping. What it
#     actually measures is gvfs. One test binary, one machine, glib 2.88.3
#     throughout: without gvfs it FAILED, with gvfs installed it passed, and
#     with gvfs installed but no session bus it FAILED again. So it needs
#     libgvfsdbus.so in the GIO module directory *and* a bus to talk to, and a
#     mock chroot has neither.
# **One physical line, and this is the whole of a bug that made %%check a
# no-op.** Written across four lines with trailing backslashes, rpm's macro
# argument capture turned each continuation into a literal `' '` argument:
#
#   cargo test ... --features ... -- ' ' --skip secrets::tests::...
#
# libtest took that space as the name filter, matched nothing, and every test
# binary reported `0 passed; N filtered out`. The build went green having run
# no tests at all -- the exact shape of failure this project keeps retracting
# commits for. Do not reflow this line.
%cargo_test -f cordial-shell/webview,cordial-runtime/webview -- -- --skip secrets::tests::a_session_survives_the_round_trip_through_the_service --skip secrets::tests::a_plaintext_store_is_adopted_and_destroyed --skip deep_link::tests::gio_reshapes_a_roblox_link_and_is_therefore_not_where_the_string_comes_from

%files
%license LICENSE
%license libbadcpu-MIT.txt
%license mcpelauncher-linker-MIT.txt
%license aosp-NOTICE.txt
%license libjnivm-MIT.txt
%license NOTICE
%license mocktail-webview-Apache-2.0.txt
%{_datadir}/cordial/plugins/
%doc README.md THIRD-PARTY-NOTICES.md
%{_bindir}/cordial-shell
%{_bindir}/cordial
%{_bindir}/cordial-run
%{_datadir}/applications/io.github.luohoa97.Cordial.desktop
%{_datadir}/metainfo/io.github.luohoa97.Cordial.metainfo.xml
%{_datadir}/icons/hicolor/scalable/apps/io.github.luohoa97.Cordial.svg
%{_datadir}/icons/hicolor/scalable/apps/io.github.luohoa97.Cordial.Frostbite.svg

%changelog
* Sat Aug 22 2026 luohoa97 <luohoa97@users.noreply.github.com> - 0.6.0-1.108.20260822git9d9c980
- Initial Copr packaging, built and tested against fedora-toolbox:44.
