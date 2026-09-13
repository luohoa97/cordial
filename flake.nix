{
  # A shell that can BUILD Cordial, and a package output that installs it.
  #
  # The devShell exists because the build quietly compiles different code
  # depending on what happens to be installed: `native/CMakeLists.txt` probes
  # for libpipewire-0.3 and libspa-0.2 and omits the audio backend when they
  # are absent, and WebKitGTK will be the same story. Two people on the same
  # commit can therefore produce binaries with different features and neither
  # has any way to tell. For a project whose whole method is "verify by
  # running", a measurement is worth what you can say about the thing you
  # measured, so the toolchain being pinned matters more here than it would
  # elsewhere.
  #
  # It is also the difference between a shell and a reboot on an immutable base
  # like Fedora Silverblue, where `dnf install` means layering onto the host
  # image for one project's build dependency.
  #
  # `packages.default` / `packages.cordial` below is a separate concern: an
  # actual installable Cordial for anyone on Nix, alongside the Flatpak,
  # AppImage, deb, rpm and AUR builds. It is INFERRED, not verified — see the
  # comment on `cordial` below for exactly what could and could not be checked
  # and why.
  #
  # Deliberately out of scope:
  #
  #   * Roblox. Cordial ships no Roblox code and never will. This pins the
  #     toolchain, not the input; you still supply an APK yourself, and
  #     CONTRIBUTING.md explains the least fiddly way to obtain one.

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        lib = pkgs.lib;

        # Cordial loads Roblox's official Android **x86-64** build natively —
        # no emulation, no CPU translation (see docs/multiarch.md and the
        # rpm spec's `ExclusiveArch: x86_64`). A package for any other system
        # cannot run the one thing this project exists to run, so rather than
        # `eachDefaultSystem` quietly producing an aarch64-linux or a Darwin
        # `packages.default` that builds and then cannot load anything, the
        # package and app outputs below are gated to this one system and
        # everything else gets `{}`. The devShell is NOT gated — it only
        # builds the tree, which is architecture-general even though the
        # binary it produces is not.
        isPackageableSystem = system == "x86_64-linux";

        cordial = pkgs.rustPlatform.buildRustPackage rec {
          pname = "cordial";
          # Read rather than duplicated, so this cannot drift from the
          # workspace the way packaging/rpm/cordial.spec's hand-maintained
          # %%global version has already been caught doing.
          version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;

          # The working tree, exactly as `git ls-files` sees it — same
          # source Cargo uses for `cargo build --release` on the host.
          #
          # **This is also the biggest open question in this file, and it is
          # a packaging concern rather than a build one.** `third_party/
          # mcpelauncher-linker` and `third_party/libjnivm` are git
          # submodules (.gitmodules), and `crates/cordial-linker-sys/
          # build.rs` panics outright if the first one's content is not on
          # disk. A `path:`/implicit-`.` flake reference and a plain
          # `github:owner/repo` reference do NOT fetch submodule content
          # unless the caller asks for it with a `?submodules=1` query
          # parameter on the flake URL — that is Nix's fetcher behaviour, not
          # something this file can default on the caller's behalf. A user
          # who runs `nix run github:luohoa97/cordial` without it will hit
          # the `preConfigure` check below with a clear message rather than
          # the build.rs panic's `git submodule update --init --recursive`,
          # which cannot be run inside the sandbox (no network, no `.git`).
          # See the README snippet in this change's report for the exact
          # invocation a user needs.
          src = self;

          preConfigure = ''
            if [ ! -f third_party/mcpelauncher-linker/bionic/linker/linker.cpp ]; then
              echo "error: third_party/mcpelauncher-linker (a git submodule) has no content in this source tree." >&2
              echo "Flakes do not fetch submodules by default. Re-run with the flake URL suffixed" >&2
              echo '  ?submodules=1  (e.g. nix run "github:luohoa97/cordial?submodules=1")' >&2
              echo "or, from a local checkout, run: git submodule update --init --recursive" >&2
              exit 1
            fi
            if [ ! -f third_party/libjnivm/CMakeLists.txt ]; then
              echo "error: third_party/libjnivm (a git submodule) has no content in this source tree; see above." >&2
              exit 1
            fi
          '';

          # The committed lockfile, not a `cargoHash`. Checked: nothing in
          # `Cargo.lock` is a `source = "git+...` dependency (`grep -c
          # 'source = "git' Cargo.lock` is 0), so there is no
          # `outputHashes` entry to keep in sync with it either — the two-line
          # case this mechanism has trouble with does not arise here.
          #
          # This is a different mechanism from `packaging/cargo-sources.json`,
          # which is Flatpak's own vendoring format for `flatpak-builder`'s
          # offline build step; the two do not share data and updating one
          # does not update the other.
          cargoLock.lockFile = ./Cargo.lock;

          # AOSP bionic does not build with GCC — see
          # native/CMakeLists.txt's FATAL_ERROR guard, which checks
          # `CMAKE_CXX_COMPILER_ID` and refuses anything that is not Clang.
          # `clangStdenv` alone does not guarantee a binary literally named
          # `clang`/`clang++` is what ends up on PATH under every nixpkgs
          # configuration, and `crates/cordial-linker-sys/build.rs` passes
          # those two literal names to CMake rather than deferring to
          # `$CC`/`$CXX` — so `clang` is also listed explicitly below,
          # exactly as the devShell already does.
          stdenv = pkgs.clangStdenv;

          nativeBuildInputs = with pkgs; [
            clang
            cmake
            pkg-config
            wrapGAppsHook4
          ];

          # No `libclang` / `LIBCLANG_PATH` here, and that was checked rather
          # than assumed: nothing in this workspace calls `bindgen` — the only
          # hit for the string anywhere in `Cargo.lock` is `wasm-bindgen`,
          # pulled in transitively by an unrelated dependency, which is a
          # different crate doing a different job and needs no libclang.
          # `crates/cordial-linker-sys` gets its bindings from hand-written
          # `extern "C"` declarations over the `cmake`-built static libraries
          # (see `build.rs`), not from bindgen over a header.
          buildInputs = with pkgs; [
            # gtk4-sys and libadwaita-sys link against the system libraries
            # rather than vendoring them, so these have to be present to link
            # at all, not merely to run — same set the devShell already
            # lists, for the same reason (ADR-011, ADR-002).
            #
            # Version floor: cordial-shell/Cargo.toml pins `gtk4 0.11` with
            # feature `v4_20` and `libadwaita 0.9` with `v1_8` — i.e. GTK
            # 4.20+ and libadwaita 1.8+ at the C library level, not just the
            # Rust binding version. This project's own floor, not one raised
            # here. **Not verified
            # against this flake's actual pinned nixpkgs revision** — there is
            # no committed `flake.lock`, `/nix/store` on this machine is
            # read-only, and no substituter is reachable from here, so
            # `nix eval nixpkgs#gtk4.version` could not be run. Reasoned
            # instead from released-software history: GTK 4.20 and
            # libadwaita 1.8 both shipped with GNOME 48 in March 2025, and
            # nixos-unstable has tracked GNOME's stable releases within weeks
            # of each one since long before that — so nixos-unstable resolved
            # today (2026-09) should clear both floors comfortably, likely by
            # more than one full GNOME cycle. That is INFERRED, not measured;
            # run `nix flake lock` (needs network) and then
            # `nix eval .#packages.x86_64-linux.default.buildInputs` /
            # `pkg-config --modversion gtk4-4.0` inside `nix develop` to
            # check it against the revision this flake actually resolves to.
            gtk4
            libadwaita
            glib
            gdk-pixbuf
            cairo
            pango
            graphene
            wayland
            libxkbcommon

            # `org.gnome.desktop.interface`'s `color-scheme` key, read by
            # `cordial-shell/src/lib.rs` to follow the system dark-mode
            # setting. Degrades gracefully without it —
            # `gio::SettingsSchemaSource::lookup` returning `None` is checked
            # before the read — so this is a quality-of-life addition rather
            # than a hard requirement, unlike everything else in this list.
            gsettings-desktop-schemas

            # Unlike the devShell, these are not optional here. The devShell
            # exists to let the audio backend and the web view silently
            # disappear or not depending on what is installed, which is
            # exactly the ambiguity a *packaged* build must not have — every
            # other format (Flatpak, deb, rpm, AUR) builds both in
            # unconditionally and then checks with `readelf -d ... | grep -i
            # webkit` that it actually linked. This derivation does the same:
            # `cargoBuildFeatures` below turns both features on, and these
            # two supply the headers `native/CMakeLists.txt` looks for so
            # the real implementation compiles rather than the honest
            # "unavailable" fallback arm.
            pipewire
            webkitgtk_6_0

            # Headers only, same reasoning as pipewire/webkitgtk above and
            # matching packaging/rpm/cordial.spec's BuildRequires list:
            # native/CMakeLists.txt compiles the real ALSA/PulseAudio backend
            # when it finds these and the honest "unavailable" arm when it
            # does not. `libpipewire-0.3.so.0`, `libasound.so.2` and
            # `libpulse.so.0` are all dlopen'd at run time rather than linked
            # (see pipewire_backend.cpp / alsa_backend.cpp / pulse_backend.cpp),
            # which is why the *runtime* library path also needs them — see
            # `preFixup` below.
            alsa-lib
            libpulseaudio

            # `-lz` on cordial-linker-sys's native link line
            # (`cargo:rustc-link-lib=dylib=z`); glibc does not bundle zlib.
            zlib
          ];

          # Both crates' `webview` feature, and both, not one. The shell
          # holds the WebKit window and `cordial-runtime` holds the presenter
          # that calls it, so enabling only the shell's leaves the caller
          # cfg'd out, the linker collects `webview::open`, and the binary
          # links no WebKitGTK at all — silently, the build still succeeds.
          # That exact shape shipped once in the Flatpak
          # (packaging/io.github.luohoa97.Cordial.yml's own comment on this
          # line) and was reported as "webview doesnt work in cordial
          # flatpak". This package makes the same choice every other
          # packaging script here makes: build the full client, not the
          # host-only default a bare `cargo build` on a headers-less machine
          # would silently fall back to.
          cargoBuildFeatures = [ "cordial-shell/webview" "cordial-runtime/webview" ];

          # `cargo test --workspace` is not run as part of this build.
          # `packaging/rpm/cordial.spec`'s own `%check` already had to skip
          # three tests by name for environments this sandboxed either —
          # two secrets-service round-trip tests that need a live
          # `org.freedesktop.secrets` on the session bus, and one GIO
          # URI-reshaping test that needs both `gvfs` and a session bus. The
          # Nix build sandbox has neither a session bus nor a display, same
          # as the mock chroot that spec was written against, so the same
          # tests would fail here for the same reason and for no fault of the
          # code. Rather than reproduce that skip list here and have it drift
          # from the rpm spec's, testing is left to `cargo test --workspace`
          # inside `nix develop`, which is what AGENTS.md already asks for
          # before trusting a change.
          doCheck = false;

          # Neither this nor the CMake build reach the network — no
          # `--share=network`-equivalent exists for a Nix build sandbox in
          # the first place, so there is nothing to disable here the way
          # packaging/io.github.luohoa97.Cordial.yml's build-commands do
          # deliberately for reproducibility. Worth restating anyway: Cordial
          # ships no Roblox code, this derivation fetches none, and nothing
          # here tries to.

          CORDIAL_GIT_SHA =
            # Best-effort provenance, never the version — see
            # crates/cordial-shell/src/version.rs's own module comment for
            # why those are two separate facts. `self.rev`/`self.shortRev`
            # exist only when the flake was fetched from a clean git commit;
            # `self.dirtyShortRev` exists for a dirty tree fetched with
            # `?submodules=1` from a local path. Neither exists for a source
            # tarball, and `""` here is deliberately the same "supply
            # nothing" signal `crates/cordial-shell/build.rs` already treats
            # as absent — filtered out by its own `.filter(|s| !s.is_empty())`
            # — so a build with no git identity present degrades to the
            # documented, correct behaviour: a clean "Cordial ${version}"
            # with no parenthesised commit, not a build error.
            self.shortRev or self.dirtyShortRev or "";

          # GTK/GLib/etc. env vars (GSETTINGS_SCHEMAS_PATH,
          # GDK_PIXBUF_MODULE_FILE, icon and schema search paths) are
          # `wrapGAppsHook4`'s job and it does this automatically for every
          # buildInput above that carries a setup hook for it — that is the
          # normal, and preferred, way a GTK4 app is wrapped in nixpkgs.
          #
          # What it does NOT cover is everything dlopen'd rather than linked:
          # the Vulkan loader `cordial_screenshot` and the engine's own
          # renderer read through
          # (docs/adr/ADR-019-development-control-surface.md; GLES2+EGL is
          # the load-bearing path per the Flatpak manifest's own comment, with
          # Vulkan dlopen'd and optional), plus the three optional audio
          # backends above. None of those get an RPATH entry from the Rust
          # linker, because none of them are ever passed to it — dlopen has no
          # link-time footprint to patch. So they go on `LD_LIBRARY_PATH`
          # instead, appended (not replacing) via the same `gappsWrapperArgs`
          # array `wrapGAppsHook4` already populates, which is the standard
          # way to add to a wrapGAppsHook wrapper without fighting it.
          #
          # `deno` goes on `PATH` for the same reason
          # packaging/rpm/cordial.spec explains it could NOT: plugins are Deno
          # programs (ADR-008) and Cordial bundles no runtime, so without
          # `deno` reachable every plugin fails to spawn. The rpm spec says
          # outright "there is no `deno` package" for Fedora or Debian: nixpkgs
          # has one, so this is one thing a Nix install can offer that the
          # other four packaging formats here currently cannot.
          preFixup = ''
            gappsWrapperArgs+=(
              --prefix LD_LIBRARY_PATH : "${lib.makeLibraryPath (with pkgs; [
                vulkan-loader
                pipewire
                libpulseaudio
                alsa-lib
              ])}"
              --prefix PATH : "${lib.makeBinPath [ pkgs.deno ]}"
            )
          '';

          postInstall = ''
            # `cordial`, not just `cordial-shell` — the same alias
            # packaging/deb/build-deb.sh and packaging/rpm/cordial.spec both
            # add, and for the same reason given there: "nobody wants to type
            # the second word", and a symlink rather than a rename keeps
            # anything already invoking `cordial-shell` by name working.
            # `cordial-run` deliberately gets no alias — it is the loader the
            # shell launches, found as `current_exe`'s sibling, and is not
            # what anyone should run by hand.
            ln -sf cordial-shell "$out/bin/cordial"

            # First-party plugins, read-only beside the binary — the same
            # install performed by every other packaging script here.
            # `system_plugin_dir` only ever looks in a packaged location
            # ($out/share/cordial/plugins, or /app/share/... under Flatpak),
            # so without this a Nix-installed Cordial's settings window would
            # list no built-in plugins at all, same bug this project already
            # shipped once for deb/rpm before those scripts existed.
            for plugin in plugins/*/; do
              id=$(basename "$plugin")
              [ -f "$plugin/plugin.json" ] || continue
              install -Dm644 "$plugin/plugin.json" "$out/share/cordial/plugins/$id/plugin.json"
              install -Dm644 "$plugin/main.ts" "$out/share/cordial/plugins/$id/main.ts"
            done

            install -Dm644 packaging/icons/hicolor/scalable/apps/io.github.luohoa97.Cordial.svg \
              "$out/share/icons/hicolor/scalable/apps/io.github.luohoa97.Cordial.svg"
            # Frostbite, the twice-a-year joke in
            # crates/cordial-shell/src/branding.rs — installed unconditionally
            # for the same reason every other packaging script here installs
            # it unconditionally: the alternative is a name that resolves to
            # nothing on the one day nobody is watching for it.
            install -Dm644 packaging/icons/hicolor/scalable/apps/io.github.luohoa97.Cordial.Frostbite.svg \
              "$out/share/icons/hicolor/scalable/apps/io.github.luohoa97.Cordial.Frostbite.svg"
            install -Dm644 packaging/io.github.luohoa97.Cordial.desktop \
              "$out/share/applications/io.github.luohoa97.Cordial.desktop"
            install -Dm644 packaging/io.github.luohoa97.Cordial.metainfo.xml \
              "$out/share/metainfo/io.github.luohoa97.Cordial.metainfo.xml"

            # Licence and attribution, travelling with the binary rather than
            # only with the source tree — MIT requires its notice in "all
            # copies or substantial portions", Apache-2.0 section 4(d)
            # requires NOTICE to travel with derivative works. Same set the
            # Flatpak manifest and the deb/rpm scripts install.
            install -Dm644 LICENSE "$out/share/licenses/cordial/LICENSE"
            install -Dm644 THIRD-PARTY-NOTICES.md "$out/share/licenses/cordial/THIRD-PARTY-NOTICES.md"
            install -Dm644 NOTICE "$out/share/licenses/cordial/NOTICE"
            install -Dm644 third_party/libbadcpu/LICENSE.upstream \
              "$out/share/licenses/cordial/libbadcpu-MIT.txt"
            install -Dm644 third_party/mcpelauncher-linker/LICENSE \
              "$out/share/licenses/cordial/mcpelauncher-linker-MIT.txt"
            install -Dm644 third_party/mcpelauncher-linker/core/NOTICE \
              "$out/share/licenses/cordial/aosp-NOTICE.txt"
            install -Dm644 third_party/libjnivm/LICENSE \
              "$out/share/licenses/cordial/libjnivm-MIT.txt"
            install -Dm644 third_party/mocktail-webview/LICENSE \
              "$out/share/licenses/cordial/mocktail-webview-Apache-2.0.txt"
          '';

          meta = {
            description = "Run Roblox natively on Linux -- you supply the Roblox build, none is shipped";
            homepage = "https://github.com/luohoa97/cordial";
            license = lib.licenses.gpl3Plus;
            mainProgram = "cordial-shell";
            # Real, not a formality — see `isPackageableSystem` above. The
            # engine this loads is Roblox's Android **x86-64** build, run
            # with no emulation, so a `libroblox.so` built for any other
            # architecture is not a thing this package could ever run even
            # if it built.
            platforms = [ "x86_64-linux" ];
          };
        };
      in
      {
        devShells.default = pkgs.mkShell {
          # Clang, not GCC, and this is not a preference: the vendored AOSP
          # bionic linker does not build with GCC at all.
          stdenv = pkgs.clangStdenv;

          nativeBuildInputs = with pkgs; [
            clang
            cmake
            pkg-config
            rustc
            cargo
            just
          ];

          buildInputs = with pkgs; [
            # gtk4-sys and libadwaita-sys link against the system libraries
            # rather than vendoring them, so these have to be present to link
            # at all, not merely to run. ADR-011 and ADR-002.
            gtk4
            libadwaita
            glib
            gdk-pixbuf
            cairo
            pango
            graphene
            wayland
            libxkbcommon

            # Optional at build time, and that is exactly the problem this
            # shell solves: without them the tree still compiles, it just
            # silently loses the audio backend and the web views. Including
            # them means everyone builds the same Cordial.
            pipewire
            webkitgtk_6_0
          ];

          shellHook = ''
            echo "cordial: clang $(clang --version | head -1 | grep -o '[0-9.]*' | head -1), rust $(rustc --version | cut -d' ' -f2)"
            echo "  pipewire  $(pkg-config --modversion libpipewire-0.3 2>/dev/null || echo MISSING)"
            echo "  webkitgtk $(pkg-config --modversion webkitgtk-6.0 2>/dev/null || echo MISSING)"
            echo "  gtk4      $(pkg-config --modversion gtk4 2>/dev/null || echo MISSING)"
            echo "  libadwaita $(pkg-config --modversion libadwaita-1 2>/dev/null || echo MISSING)"
            echo
            echo "This shell builds Cordial. Run it on the host: just dev"
          '';
        };
      }
      // lib.optionalAttrs isPackageableSystem {
        packages.default = cordial;
        packages.cordial = cordial;

        apps.default = {
          type = "app";
          program = "${cordial}/bin/cordial-shell";
        };
      });
}
