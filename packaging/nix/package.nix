# pkgs/by-name/co/cordial/package.nix
#
# DRAFT, derived from the flake.nix in the cordial repository, which was built
# on 2026-10-01. See README.txt beside this file for what a reviewer will ask.
{
  lib,
  clangStdenv,
  rustPlatform,
  fetchFromGitHub,
  cmake,
  pkg-config,
  wrapGAppsHook4,
  gtk4,
  libadwaita,
  glib,
  gdk-pixbuf,
  cairo,
  pango,
  graphene,
  wayland,
  libxkbcommon,
  gsettings-desktop-schemas,
  pipewire,
  webkitgtk_6_0,
  alsa-lib,
  libpulseaudio,
  zlib,
  vulkan-loader,
  deno,
}:

# AOSP bionic does not build with GCC; native/CMakeLists.txt refuses a non-Clang
# compiler outright.
rustPlatform.buildRustPackage.override { stdenv = clangStdenv; } (finalAttrs: {
  pname = "cordial";
  version = "0.23.2";

  src = fetchFromGitHub {
    owner = "luohoa97";
    repo = "cordial";
    tag = "v${finalAttrs.version}";
    # third_party/mcpelauncher-linker (with its own nested bionic and core) and
    # third_party/libjnivm are git submodules and the build fails without them.
    fetchSubmodules = true;
    hash = "sha256-u0upyWA5xG6zd9hvvP8Mh5ScwRwuz5xLJaY28h3523k=";
  };

  cargoHash = "sha256-etdVYyzOiBTCzZ2TuQpWwD0580UjHfahLd0wZ5w7YCk=";

  nativeBuildInputs = [
    cmake # driven by crates/cordial-linker-sys/build.rs, not by the setup hook
    pkg-config
    wrapGAppsHook4
  ];

  buildInputs = [
    # linked, via the -sys crates' pkg-config probes
    gtk4
    libadwaita
    glib
    gdk-pixbuf
    cairo
    pango
    graphene
    wayland
    libxkbcommon
    gsettings-desktop-schemas
    zlib
    # The audio backends and the web view are compiled only if their headers
    # are found, and are otherwise silently replaced by an "unavailable" stub.
    # These are therefore required, not optional.
    pipewire
    webkitgtk_6_0
    alsa-lib
    libpulseaudio
  ];

  # Both crates, not one: with only the shell's feature the caller in
  # cordial-runtime is cfg'd out, the linker drops webview::open, and the
  # build succeeds with no WebKitGTK linked. It is `buildFeatures`;
  # `cargoBuildFeatures` is overwritten by buildRustPackage.
  #
  # Not `cordial-runtime/vr` (Play in VR, ADR-053): this pins v0.23.2, which
  # predates that feature. When the pin moves to a tag that has it, add the
  # feature here and `boost` to the inputs; the flake.nix in the repository
  # already does.
  buildFeatures = [
    "cordial-shell/webview"
    "cordial-runtime/webview"
  ];

  # Upstream skips three tests that need a session bus (two secret-service
  # round trips and a GIO URI test) when it runs them, and the rest were not
  # run for this draft. TODO: try `cargo test --workspace` with those skipped.
  doCheck = false;

  preFixup = ''
    # Things Cordial dlopen()s have no link-time footprint to patch.
    gappsWrapperArgs+=(
      --prefix LD_LIBRARY_PATH : "${
        lib.makeLibraryPath [
          vulkan-loader
          pipewire
          libpulseaudio
          alsa-lib
        ]
      }"
      # Plugins are Deno programs and Cordial bundles no runtime.
      --prefix PATH : "${lib.makeBinPath [ deno ]}"
    )
  '';

  postInstall = ''
    # Fails the build if the webview features did not take.
    readelf -d "$out/bin/cordial-run" | grep -qi webkit

    ln -s cordial-shell "$out/bin/cordial"

    for plugin in plugins/*/; do
      id=$(basename "$plugin")
      [ -f "$plugin/plugin.json" ] || continue
      install -Dm644 "$plugin/plugin.json" "$out/share/cordial/plugins/$id/plugin.json"
      install -Dm644 "$plugin/main.ts" "$out/share/cordial/plugins/$id/main.ts"
    done

    install -Dm644 packaging/icons/hicolor/scalable/apps/io.github.luohoa97.Cordial.svg \
      "$out/share/icons/hicolor/scalable/apps/io.github.luohoa97.Cordial.svg"
    install -Dm644 packaging/icons/hicolor/scalable/apps/io.github.luohoa97.Cordial.Frostbite.svg \
      "$out/share/icons/hicolor/scalable/apps/io.github.luohoa97.Cordial.Frostbite.svg"
    install -Dm644 packaging/io.github.luohoa97.Cordial.desktop \
      "$out/share/applications/io.github.luohoa97.Cordial.desktop"
    install -Dm644 packaging/io.github.luohoa97.Cordial.metainfo.xml \
      "$out/share/metainfo/io.github.luohoa97.Cordial.metainfo.xml"

    # Notices that upstream asks to travel with the binary.
    install -Dm644 LICENSE NOTICE THIRD-PARTY-NOTICES.md -t "$out/share/licenses/cordial"
    install -Dm644 third_party/libbadcpu/LICENSE.upstream "$out/share/licenses/cordial/libbadcpu-MIT.txt"
    install -Dm644 third_party/mcpelauncher-linker/LICENSE "$out/share/licenses/cordial/mcpelauncher-linker-MIT.txt"
    install -Dm644 third_party/mcpelauncher-linker/core/NOTICE "$out/share/licenses/cordial/aosp-NOTICE.txt"
    install -Dm644 third_party/libjnivm/LICENSE "$out/share/licenses/cordial/libjnivm-MIT.txt"
    install -Dm644 third_party/mocktail-webview/LICENSE "$out/share/licenses/cordial/mocktail-webview-Apache-2.0.txt"
  '';

  meta = {
    description = "Run Roblox natively on Linux; you supply the Roblox build, none is shipped";
    homepage = "https://github.com/luohoa97/cordial";
    changelog = "https://github.com/luohoa97/cordial/blob/v${finalAttrs.version}/CHANGELOG.md";
    # Cordial itself is GPL-3.0-or-later; statically linked vendored code is MIT
    # (mcpelauncher-linker, libjnivm, libbadcpu) and Apache-2.0 (mocktail-webview).
    license = with lib.licenses; [
      gpl3Plus
      mit
      asl20
    ];
    mainProgram = "cordial";
    # Loads Roblox's Android x86-64 build natively, with no emulation.
    platforms = [ "x86_64-linux" ];
    # A maintainer is needed. The upstream author has not agreed to maintain a
    # nixpkgs package, so this is deliberately empty rather than guessed.
    maintainers = [ ];
  };
})
