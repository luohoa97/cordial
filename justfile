# Development shortcuts.
#
# `just dev` starts the shell, which is what a user starts — the shell is the
# thing that finds a Roblox build, explains itself when there is not one, and
# launches the client. `just client` skips it and runs the engine directly,
# which is what a debugging session wants and nobody else does.

_default:
    @just --list

# Reclaim disk: just clean [all]   (build artifacts only; never a profile or a cache of Roblox)
clean what="":
    #!/usr/bin/env bash
    set -euo pipefail
    # Written after a session filled a 728 GB disk to 99% across four target
    # directories and one Flatpak build tree, and the failure did not look like
    # a disk failure. Cargo cannot finish writing an rlib, and the next build
    # reports `can't find crate for winnow` — so it reads as a broken toolchain
    # or a corrupt registry, and the fix people reach for is reinstalling one of
    # those. The actual fix is `df` and then deleting a target directory.
    #
    # Deliberately NOT touched, however much space they hold:
    #   ~/.local/share/cordial/profiles/*   a user's saved sessions and identity
    #   ~/.cache/cordial-known-good/*       the last engine known to work, which
    #                                       matters more than usual while the
    #                                       updater has no fallback of its own
    #                                       (TASKS.md U2)
    # Both are recoverable only by re-downloading or re-signing-in, and neither
    # is a build artifact.
    for d in target-distrobox target-toolbox target-nix target-jnitrace .flatpak-builder; do
        [ -e "$d" ] || continue
        printf '  %-18s %s\n' "$d" "$(du -sh "$d" 2>/dev/null | cut -f1)"
        rm -rf "$d"
    done
    # `target` last and only when asked: it is the host build, it is the biggest,
    # and losing it costs a full rebuild rather than the minute the others cost.
    if [ "{{ what }}" = "all" ] && [ -e target ]; then
        printf '  %-18s %s\n' "target" "$(du -sh target 2>/dev/null | cut -f1)"
        rm -rf target
    elif [ -e target ]; then
        echo "  target ($(du -sh target 2>/dev/null | cut -f1)) kept — 'just clean all' removes it too"
    fi
    df -h . | tail -1

# Build: just build [host|toolbox|distrobox|nix]  (Clang required; bionic will not build with GCC)
build where="host":
    #!/usr/bin/env bash
    set -euo pipefail
    # Each environment gets its own CARGO_TARGET_DIR. They have different
    # toolchains and different libraries visible, so sharing target/ between
    # them means every switch is a full rebuild at best, and at worst a binary
    # linked against headers from one and libraries from the other.
    #
    # A container-built binary still runs on the host: distrobox shares the
    # filesystem, and the libraries this needs are present on the host already —
    # webkitgtk in particular is installed there as a runtime library, and only
    # its *headers* were missing, which is why building outside the host works
    # and running outside it is unnecessary.
    case "{{ where }}" in
      host)
        cargo build --release
        ;;
      distrobox)
        # The optional dependencies are the point of this one. native/CMakeLists.txt
        # silently omits the audio backend when libpipewire-0.3 is absent, and the
        # web views want webkitgtk6.0-devel, both of which mean `dnf install` and a
        # reboot on an immutable host. A container is neither.
        # Superseded by `toolbox` above, which pins the image and installs a
        # known dependency set. Kept because people already have a
        # `my-distrobox`: it builds with whatever that container carries, and on
        # a Fedora 43 image that is gtk4 4.20.3 against the 4.22 this workspace
        # pins, which dies in three *-sys build scripts.
        box="${CORDIAL_DISTROBOX:-my-distrobox}"
        # Not `grep -q`. It exits at the first match and closes the pipe, which
        # SIGPIPEs `distrobox list`, and `set -o pipefail` then reports the whole
        # pipeline as failed — so the check inverted and claimed a container that
        # plainly exists did not. Plain grep reads all of its input.
        if ! distrobox list 2>/dev/null | grep "| $box " >/dev/null; then
            echo "no distrobox called '$box' (set CORDIAL_DISTROBOX to pick another)" >&2
            distrobox list 2>/dev/null >&2 || true
            exit 1
        fi
        # The `webview` features, for the same reason `toolbox` below passes
        # them: this recipe's own comment says the web views are the point of
        # building in a container, and it then built without them for as long
        # as it has existed. A container that carries webkitgtk6.0-devel and
        # produces a binary with no libwebkitgtk in it is the worst of both --
        # the dependency was installed, the build succeeded, and `openWindow`
        # still did nothing.
        distrobox enter "$box" -- bash -lc \
          'CARGO_TARGET_DIR=target-distrobox cargo build --release --features cordial-shell/webview,cordial-runtime/webview'
        echo "built into target-distrobox/release"
        ;;
      toolbox)
        # Cordial's own container, and the recommended way to build. `distrobox`
        # below predates it and is kept working for anyone who already has one.
        #
        # It exists because the two things the full client needs are exactly the
        # two an immutable host will not give you without layering a package and
        # rebooting: `webkitgtk6.0-devel` for the in-experience web window, and
        # `libpipewire-0.3` headers for the audio backend, which
        # native/CMakeLists.txt otherwise omits in silence.
        #
        # Fedora 44 specifically. Fedora 43 carries gtk4 4.20.3 and libadwaita
        # 1.8.3 against the 4.22 / 1.9 this workspace pins, and the failure is
        # three *-sys build scripts dying with a pkg-config hint that reads as a
        # missing package rather than an old one.
        #
        # To create it:
        #   distrobox create --name cordial \
        #     --image registry.fedoraproject.org/fedora-toolbox:44 --yes
        #   distrobox enter cordial -- sudo dnf install -y \
        #     gtk4-devel libadwaita-devel webkitgtk6.0-devel pipewire-devel \
        #     clang cmake gcc-c++ pkgconf-pkg-config openssl-devel \
        #     wayland-devel vulkan-loader-devel libxkbcommon-devel \
        #     binutils llvm lld librsvg2-tools patchelf
        #
        # binutils is not optional and not obvious: clang arrives without `ar`,
        # and CMake's static-library step then fails with "Error running link
        # command: no such file or directory", which names neither the tool nor
        # the package.
        #
        # The last two are for packaging/appimage/build-appimage.sh rather than
        # for `just build`, and were added on 2026-08-27 when the first attempt
        # to run that script in this container stopped at `rsvg-convert is not
        # installed`. patchelf matters more than it looks: linuxdeploy carries
        # its own copy from 2022, which relocates .init without updating
        # DT_INIT on any library with a .relr.dyn section -- every Fedora 44
        # one -- and the AppImage then dies in the dynamic loader before main
        # with nothing printed.
        box="${CORDIAL_TOOLBOX:-cordial}"
        if ! distrobox list 2>/dev/null | grep "| $box " >/dev/null; then
            echo "no container called '$box' (set CORDIAL_TOOLBOX to pick another)" >&2
            echo "create it with the commands in this recipe's comment" >&2
            exit 1
        fi
        # Both crates' `webview` features, not only the shell's. The shell holds
        # the WebKit window and `cordial-runtime` holds the presenter that calls
        # it, so enabling one leaves the caller cfg'd out, nothing references
        # `webview::open`, the linker collects it, and the binary carries no
        # libwebkitgtk at all. That is the same dead-code shape that made web
        # views silently do nothing for months, reappearing one layer up.
        # `readelf -d target-toolbox/release/cordial-run | grep -i webkit` is the
        # check that it actually linked.
        distrobox enter "$box" -- bash -lc \
          'CARGO_TARGET_DIR=target-toolbox cargo build --release --features cordial-shell/webview,cordial-runtime/webview'
        echo "built into target-toolbox/release"
        ;;
      nix)
        # Unverified on an ostree host, where /nix is part of the read-only image
        # and nix-daemon is disabled. See CONTRIBUTING.md.
        nix develop --command bash -c \
          'CARGO_TARGET_DIR=target-nix cargo build --release'
        echo "built into target-nix/release"
        ;;
      *)
        echo "usage: just build [host|distrobox|nix]" >&2
        exit 1
        ;;
    esac

# Run the workspace tests
test:
    cargo test --workspace

# Build and test, the pre-pull-request gate
check: build test

# Start Cordial: just dev [--in host|toolbox|distrobox|nix] [--apk PATH] [--play]
dev *args:
    #!/usr/bin/env bash
    set -euo pipefail
    # A shebang recipe gets its arguments substituted into the script text rather
    # than handed to it as $@, so they have to be planted before anything can
    # parse them. Without this every invocation behaves as though it were given
    # no arguments, which looks like a broken recipe rather than a missing
    # `set --`.
    set -- {{ args }}
    apk="" env=host extra=()
    while [ $# -gt 0 ]; do
        case "$1" in
            --apk)   apk="${2:-}"; shift 2 ;;
            --apk=*) apk="${1#*=}"; shift ;;
            # `--in` rather than a positional, because a positional would swallow
            # `--apk` as its value — which it did, the first time this was written.
            --in)    env="${2:-}"; shift 2 ;;
            --in=*)  env="${1#*=}"; shift ;;
            # Start playing straight away, so a harness or an agent gets a
            # running client without a human pressing Roblox. Goes through the
            # window's own `win.launch` action, which is the same thing the
            # button does.
            --play)  export CORDIAL_AUTOSTART=1; shift ;;
            *)       extra+=("$1"); shift ;;
        esac
    done
    just build "$env"
    # Each build environment has its own target directory, so the runner has to
    # be told which one it is looking at. Assuming ./target meant `just build
    # distrobox` followed by `just dev` silently ran the older HOST binary — a
    # stale build that still executes is this project's most expensive kind of
    # mistake, and it has already cost two measurements of code that was never
    # under test.
    case "$env" in
      host)      bindir=target/release ;;
      toolbox)   bindir=target-toolbox/release ;;
      distrobox) bindir=target-distrobox/release ;;
      nix)       bindir=target-nix/release ;;
      *)         echo "usage: just dev [--in host|toolbox|distrobox|nix] [--apk PATH]" >&2; exit 1 ;;
    esac
    if [ ! -x "$bindir/cordial-shell" ]; then
        echo "no cordial-shell in $bindir — did 'just build $env' succeed?" >&2
        exit 1
    fi
    # Deliberately no check that the APK exists and no advice about how to get
    # one. Finding a build, and explaining what to do when there is not one, is
    # the shell's job — it is what the user actually sees, and a second copy of
    # that message here would drift out of step with it.
    if [ -n "$apk" ]; then
        export CORDIAL_APK="$apk"
    fi
    # The development control surface, on by default for `just dev` because
    # that is what `just dev` is for. It costs a socket in the profile
    # directory and nothing else until something connects, and having it off
    # by default here would mean every investigation started by restarting the
    # client -- which is exactly the friction it was built to remove. Set
    # CORDIAL_DEV_CONTROL=0 to run without it.
    #
    # Exported rather than passed as a flag because the shell launches the
    # client as a child and the child inherits the environment; `launch.rs`
    # adds to that environment rather than clearing it.
    export CORDIAL_DEV_CONTROL="${CORDIAL_DEV_CONTROL:-1}"
    if [ "$CORDIAL_DEV_CONTROL" != 0 ]; then
        echo "dev control on; attach with: just mcp" >&2
    fi
    # **Built-in plugins, from the source tree rather than from a package.**
    #
    # `manifest::system_plugin_dir` looks in `/usr/share/cordial/plugins` (or
    # `/app/share/...` in the Flatpak), which only a packaging script ever
    # populates -- so a built-in plugin could not be run at all without
    # building a .deb or an .rpm first. Reported as "built in plugins cant be
    # tested in the just dev --in toolbox versions, it has to be packaged. bad
    # for development", and it is: the loop for changing one line of a
    # first-party plugin was a full package build.
    #
    # The override already existed for distributions that install elsewhere.
    # Pointing it at `plugins/` makes `just dev` load exactly what the
    # packaging scripts would install, from the files being edited.
    export CORDIAL_SYSTEM_PLUGIN_DIR="${CORDIAL_SYSTEM_PLUGIN_DIR:-$PWD/plugins}"
    echo "built-in plugins from $CORDIAL_SYSTEM_PLUGIN_DIR" >&2
    exec "./$bindir/cordial-shell" ${extra+"${extra[@]}"}

# Run the engine directly: just client [--in host|distrobox|nix] [--apk PATH] [--run SECS]
client *args:
    #!/usr/bin/env bash
    set -euo pipefail
    set -- {{ args }}
    apk="" lib="" run="600" env=host x11=0 extra=()
    # `--apk` and `--lib-dir` are spelled the way cordial-run spells them: this
    # recipe exists to stop people assembling that command by hand, not to teach
    # a second vocabulary for it. Anything unrecognised passes straight through,
    # so --dump-classes and friends still work.
    while [ $# -gt 0 ]; do
        case "$1" in
            --apk)       apk="${2:-}"; shift 2 ;;
            --apk=*)     apk="${1#*=}"; shift ;;
            --lib-dir)   lib="${2:-}"; shift 2 ;;
            --lib-dir=*) lib="${1#*=}"; shift ;;
            --run)       run="${2:-}"; shift 2 ;;
            --run=*)     run="${1#*=}"; shift ;;
            --in)        env="${2:-}"; shift 2 ;;
            --in=*)      env="${1#*=}"; shift ;;
            --x11)       x11=1; shift ;;
            *)           extra+=("$1"); shift ;;
        esac
    done
    # Wayland unless asked otherwise, because ADR-011 makes it the target and the
    # shell has always launched with it. This recipe did not, so `just dev` and
    # `just client` ran DIFFERENT display backends, and every fix verified through
    # one was being tested through the other. X11 is still reachable with --x11
    # until ADR-011's removal trigger fires.
    # Inverted, because the runtime's default inverted. `backend()` now prefers
    # Wayland whenever `WAYLAND_DISPLAY` is set, so *not* exporting
    # CORDIAL_WAYLAND no longer selects X11 -- it selects nothing, and --x11
    # would have silently kept giving you Wayland. Asking for X11 is now an
    # explicit CORDIAL_X11, which is the only thing that still forces it.
    if [ "${x11:-0}" = 1 ]; then
        export CORDIAL_X11=1
    fi
    # Cordial ships no Roblox build. Sober downloads the same official Android
    # one this runtime loads, so checking there first means a debugging run needs
    # no arguments. The shell says all this properly to a user who has neither.
    sober_apk="$HOME/.var/app/org.vinegarhq.Sober/data/sober/packages/x86_64/com.roblox.client/base.apk"
    if [ -z "$apk" ] && [ -f "$sober_apk" ]; then
        apk="$sober_apk"
        echo "using the build Sober downloaded: $apk"
    fi
    if [ -z "$apk" ] || [ ! -f "$apk" ]; then
        [ -n "$apk" ] && echo "no APK at $apk" >&2
        echo "usage: just client --apk /path/to/base.apk    (or run \`just dev\`, which explains how to get one)" >&2
        exit 1
    fi
    [ -n "$lib" ] || lib="$(dirname "$apk")/lib/x86_64"
    if [ ! -f "$lib/libroblox.so" ]; then
        # Nothing unpacked, so unpack it. On a split build the engine is in
        # split_config.<abi>.apk rather than base.apk, so try the APK given and
        # then its siblings instead of asserting which one holds it.
        cache="${XDG_CACHE_HOME:-$HOME/.cache}/cordial/lib/x86_64"
        # Re-extract when the APK it came from has changed. Presence alone was
        # the test until now, so installing a new Roblox build left the OLD
        # engine cached and Cordial ran it against the new APK's assets --
        # a silent version mismatch, which is worse than the cold start the
        # cache exists to avoid. mtime on the extracted file cannot be used: zip
        # preserves the stored timestamp, so it reads 1981.
        stamp="$cache/.from"
        want="$(stat -c '%s %Y %n' "$apk" 2>/dev/null)"
        if [ -f "$cache/libroblox.so" ] && [ "$(cat "$stamp" 2>/dev/null)" != "$want" ]; then
            echo "the APK changed since libroblox.so was extracted; re-extracting"
            rm -f "$cache/libroblox.so"
        fi
        if [ ! -f "$cache/libroblox.so" ]; then
            mkdir -p "$cache"
            for candidate in "$apk" "$(dirname "$apk")"/split_config*.apk; do
                [ -f "$candidate" ] || continue
                if unzip -o -j -q "$candidate" 'lib/x86_64/libroblox.so' -d "$cache" 2>/dev/null \
                   && [ -f "$cache/libroblox.so" ]; then
                    printf '%s' "$want" > "$stamp"
                    echo "extracted libroblox.so from $(basename "$candidate") into $cache"
                    break
                fi
            done
        fi
        lib="$cache"
    fi
    if [ ! -f "$lib/libroblox.so" ]; then
        echo "no libroblox.so in $apk, its split_config siblings, or $lib" >&2
        echo "pass --lib-dir if the engine lives somewhere else" >&2
        exit 1
    fi
    just build "$env"
    # Whichever environment built it, run that one. Defaulting to ./target here
    # would silently run a stale host binary after a container build.
    case "$env" in
      host)      bindir=target/release ;;
      toolbox)   bindir=target-toolbox/release ;;
      distrobox) bindir=target-distrobox/release ;;
      nix)       bindir=target-nix/release ;;
      *)         echo "usage: just client [--in host|distrobox|nix] ..." >&2; exit 1 ;;
    esac
    if [ ! -x "$bindir/cordial-run" ]; then
        echo "no cordial-run in $bindir — did 'just build $env' succeed?" >&2
        exit 1
    fi
    # On for the same reason as in `just dev`: this recipe exists to be poked at.
    export CORDIAL_DEV_CONTROL="${CORDIAL_DEV_CONTROL:-1}"
    if [ "$CORDIAL_DEV_CONTROL" != 0 ]; then
        echo "dev control on; attach with: just mcp" >&2
    fi
    # Built-in plugins from the source tree, for the same reason `just dev`
    # does it -- see the longer comment there. Both recipes, because a plugin
    # that loads under one and not the other is a difference nobody would think
    # to suspect.
    export CORDIAL_SYSTEM_PLUGIN_DIR="${CORDIAL_SYSTEM_PLUGIN_DIR:-$PWD/plugins}"
    # A long default timer on purpose: a run that ends on its own while somebody
    # is still reading the screen looks exactly like a crash, and that has
    # already cost one debugging session here.
    exec "./$bindir/cordial-run" \
        --lib-dir "$lib" --apk "$apk" \
        --host-libc --game-activity --run "$run" ${extra+"${extra[@]}"}

# Put the app icons where a development build can actually resolve them: just icons
icons:
    #!/usr/bin/env bash
    set -euo pipefail
    # A `just dev` build installs nothing, so its window borrows whatever icon
    # the *installed* Flatpak exported -- which is why a dev build titled
    # Frostbite still wore the Cordial mango, and why that looked like a code
    # bug rather than a missing file.
    #
    # On Wayland the compositor resolves a window's icon by name through the
    # icon theme, so the fix is to have the names on disk. Both go in, because
    # a name that resolves to nothing gives a blank window in the switcher.
    dest="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/scalable/apps"
    mkdir -p "$dest"
    cp packaging/icons/hicolor/scalable/apps/io.github.luohoa97.Cordial.svg "$dest/"
    cp packaging/icons/hicolor/scalable/apps/io.github.luohoa97.Cordial.Frostbite.svg "$dest/"
    # Harmless if absent, and GTK re-reads scalable icons without it anyway.
    gtk-update-icon-cache -q -t -f "$(dirname "$(dirname "$dest")")" 2>/dev/null || true
    echo "installed both icons into $dest"

# Attach the development MCP to whichever running client has a control socket
mcp *args:
    #!/usr/bin/env bash
    set -euo pipefail
    # Speaks MCP on stdin/stdout, so this is what an agent's MCP configuration
    # runs rather than something to type at a prompt. Register it once with
    #
    #     claude mcp add cordial -- just mcp
    #
    # It finds the socket itself rather than being told where it is: the most
    # recently bound devctl.sock wins, so it lands on whichever client was
    # started last, whether that was `just dev`, `just client`, or a bare
    # cordial-run with CORDIAL_DEV_CONTROL=1. There is no pid to look up either
    # -- the debugger tools ask the client for its own over the socket.
    #
    # XDG_DATA_HOME is honoured because AGENTS.md asks agents to set it so
    # parallel runs do not collide on one profile lock, and such a run puts its
    # profile somewhere else entirely.
    root="${XDG_DATA_HOME:-$HOME/.local/share}/cordial"
    exec python3 tools/cordial-mcp.py --profile-root "$root" {{ args }}

# `just client` with text-entry tracing, which is the one wanted most often
client-text *args:
    @CORDIAL_TRACE_TEXT=1 just client {{ args }}

# What a new APK needs that the stub table lacks: just symbols --lib-dir DIR
symbols lib_dir:
    #!/usr/bin/env bash
    set -euo pipefail
    # AGENTS.md explains why one missing *data* symbol stops the whole client at
    # load time rather than at first use.
    new=$(mktemp) old=$(mktemp)
    trap 'rm -f "$new" "$old"' EXIT
    readelf --dyn-syms -W {{ quote(lib_dir) }}/libroblox.so \
      | awk '$7=="UND" {print $8}' | sed 's/@.*//' | sort -u > "$new"
    cut -f2 docs/analysis/undefined-symbols.tsv | sort -u > "$old"
    comm -23 "$new" "$old"

# Pull vinegarhq/sober's issue tracker into a local, triage-searchable corpus (ADR-017)
sober-corpus-fetch:
    deno run \
      --allow-net=api.github.com \
      --allow-env=GITHUB_TOKEN,GH_TOKEN \
      --allow-run=gh \
      --allow-read=tools/sober-corpus/data \
      --allow-write=tools/sober-corpus/data \
      tools/sober-corpus/fetch.ts

# Derive the triage set (problem + maintainer resolution) from the raw corpus
sober-corpus-derive:
    deno run \
      --allow-read=tools/sober-corpus/data \
      --allow-write=tools/sober-corpus/data \
      tools/sober-corpus/derive.ts
