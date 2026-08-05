//! `cordial-load` — load `libroblox.so` with the bionic linker.
//!
//! This does not run Roblox. It proves the loader, the relocations and the TLS
//! layout work against the real 116 MB object, and turns
//! docs/framework-api-inventory.md into a prioritised list of what to implement.

use std::process::ExitCode;
use std::time::Instant;

use cordial_linker_sys as linker;
use cordial_runtime::{stubs, symtab};

struct Options {
    lib_dir: String,
    library: String,
    apk: Option<String>,
    read_asset: Option<String>,
    client_settings: Option<String>,
    flag_overrides: Option<String>,
    gl_probe: bool,
    window_seconds: Option<u64>,
    game_activity: bool,
    join_url: Option<cordial_runtime::deeplink::JoinUrl>,
    run_seconds: u64,
    host_libc: bool,
    jni_onload: bool,
    dump_classes: Option<String>,
    verbose: bool,
}

const USAGE: &str = "\
usage: cordial-load --lib-dir <dir> [options]

  --lib-dir <dir>   directory holding the APK's lib/x86_64/ objects
  --library <name>  object to load (default: libroblox.so)
  --apk <path>      APK to serve assets from; without it AAssetManager_open fails
  --read-asset <p>  read one asset through the AAsset API and report its size
  --client-settings <f>  newline-free list of flag names to pre-cache.
                    NOT the ClientSettings document — the engine loads values itself
  --flag-overrides <f>  JSON passed to nativePreloadFlagOverrides. DIAGNOSTIC
                    ONLY: that native does nothing observable despite its name,
                    tested with several document shapes. To actually set a flag,
                    use ~/.config/cordial/flags.json (see CONTRIBUTING.md)
  --gl-probe        bring up GLES2 through the symbol table and read a pixel back
  --window <secs>   GL PROBE ONLY: open a window and draw a gradient for <secs>.
                    This is Cordial's own test pattern, not Roblox rendering.
  --host-libc       also resolve libc from the host (ABI-unsafe; diagnostic only)
  --jni-onload      stand up a JavaVM and call JNI_OnLoad
  --game-activity   implies --jni-onload; bring Roblox up and hand it a surface
  --join-url <url>  a roblox-player:// or roblox:// link from a browser click,
                    handed to the engine during bring-up. Rejected unless it is
                    one of those two schemes, printable ASCII, and under 2 kB.
                    A roblox-player: link in the desktop launcher's format is
                    rewritten into the roblox:// form this engine matches; its
                    one-time gameinfo ticket is dropped and never printed
  --run <secs>      how long to let Roblox run after handover (default 15).
                    0 means no timer: run until the window is closed or the
                    process is sent SIGTERM/SIGINT. Closing the window ends the
                    process either way — the timer is a backstop for headless
                    and scripted runs, not the way a session is meant to end
  --dump-classes <f>  implies --jni-onload; write the Java classes Roblox asked
                    for to <f> — the observed Phase 2 backlog
  -v, --verbose     list every symbol and how it resolved

env:
  MCPELAUNCHER_LINKER_VERBOSITY=<n>  bionic linker tracing (try 1 or 2)
  CORDIAL_STUB_ABORT=1               abort on the first unimplemented call
  CORDIAL_STUB_QUIET=1               do not report stub hits as they happen
  CORDIAL_TRACE=1                    log libc calls (WARNING: wraps variadic
                                     functions with fixed-arity ones, which is
                                     not ABI-safe — it changes behaviour)
  CORDIAL_ANDROID_TRACE=1            log Android API calls (safe; no variadics)
  CORDIAL_MONITOR=<n>                open the window on the nth monitor (0 is
                                     the first), instead of the primary one
  CORDIAL_WINDOW_POS=<x>,<y>         explicit window position; wins over
                                     CORDIAL_MONITOR
  CORDIAL_FULLSCREEN=1               cover the chosen monitor and ask the
                                     window manager for fullscreen
  CORDIAL_RESOLUTION=<w>x<h>         render resolution (default 1280x720);
                                     CORDIAL_FULLSCREEN overrides it
  CORDIAL_DPI_SCALE=<f>              UI density Roblox lays out against.
                                     1.0 is a low-density phone; try 1.5-2
  CORDIAL_PLATFORM_NAME=<name>       what Cordial answers when the engine asks
                                     which platform it is on. Defaults to Linux,
                                     one of the engine's own Enum.Platform
                                     names; =Android is the control run. See
                                     docs/analysis/platform-identity.md
  CORDIAL_WHEEL_SCALE=<f>            scroll wheel detents per notch (default 1);
                                     negative inverts the direction
  CORDIAL_TRACE_WHEEL=1              log every wheel event and the arguments
                                     nativePassMouseWheel received
  CORDIAL_NO_POINTER_LOCK=1          never capture the pointer, whatever the
                                     engine or the mouse asks for. The control
                                     for the capture path; it still polls and
                                     traces the engine's own request, so a
                                     control run says what it would have done
  CORDIAL_NO_DRAG_LOCK=1             capture only when the engine asks, not
                                     while a right/middle button is held
  CORDIAL_NO_CLOSE_EXIT=1            closing the window does not end the
                                     process — the old behaviour, kept as the
                                     control for the close path. SIGTERM and
                                     --run are unaffected
  CORDIAL_SIGNIN_PROBE=1             ask the engine whether login is Lua-rendered
  CORDIAL_DEEPLINK_PROBE=1           with --join-url, print the linking
                                     protocol's own message and field names,
                                     read out of the running engine
  CORDIAL_DEEPLINK_NO_TRANSLATE=1    hand a roblox-player:// desktop link to the
                                     engine as it arrived, instead of rewriting
                                     it into the roblox:// form the engine's own
                                     pattern matches. The control for the
                                     translation; the engine does not act on the
                                     untranslated link, which is the point
  CORDIAL_NO_VULKAN=1                make the host look like it has no Vulkan
                                     loader, forcing the GLES2/EGL fallback
                                     path Roblox uses when dlopen(libvulkan)
                                     fails
  CORDIAL_PRESENT_MODE=<m>           swapchain present mode: auto (the default;
                                     MAILBOX when the driver advertises it),
                                     off (forward the engine's own choice, which
                                     is FIFO — this is the control for a frame
                                     rate measurement), or one of mailbox,
                                     immediate, fifo, fifo-relaxed. FIFO is the
                                     only mode the spec guarantees, so anything
                                     the driver does not advertise falls back to
                                     what the engine asked for
  CORDIAL_GAMEMODE=0                 do not ask Feral GameMode to raise the CPU
                                     governor and priority for this process.
                                     On by default; a machine without gamemoded
                                     says so once and carries on
  CORDIAL_COUNT_GL=1                 count eglCreateWindowSurface/MakeCurrent/
                                     SwapBuffers/glClear/Draw*/CompileShader
                                     calls and report them after --run
  CORDIAL_SWAP_TIMES=1               with CORDIAL_COUNT_GL=1, also print how
                                     long each real eglSwapBuffers call blocked
";

fn parse() -> Result<Options, String> {
    let mut opt = Options {
        lib_dir: String::new(),
        library: "libroblox.so".into(),
        apk: None,
        read_asset: None,
        client_settings: None,
        flag_overrides: None,
        gl_probe: false,
        window_seconds: None,
        game_activity: false,
        join_url: None,
        run_seconds: 15,
        host_libc: false,
        jni_onload: false,
        dump_classes: None,
        verbose: false,
    };
    // Before anything can latch a profile. ADR-012's move used to be driven only
    // from the shell's `main`, so a client started any other way — `just client`,
    // or a hand-typed command — silently kept writing to the pre-ADR-012
    // `instances/default` while a shell-started one used `profiles/default`.
    // Signing in through one and restarting through the other then looked exactly
    // like the session being dropped. This is a no-op once the move has happened.
    cordial_runtime::profile::migrate_legacy_layout();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--lib-dir" => opt.lib_dir = args.next().ok_or("--lib-dir needs a value")?,
            "--library" => opt.library = args.next().ok_or("--library needs a value")?,
            "--apk" => opt.apk = Some(args.next().ok_or("--apk needs a path")?),
            // Which profile's storage this instance runs against. The profile is
            // an argument and the settings inside it are not, deliberately: one
            // value decides where everything else lives, and a setting passed on
            // a command line cannot change while the client runs — which the
            // dynamic DFFlag families exist precisely to do (ADR-013).
            //
            // This has to be resolved before anything reads the profile, because
            // `profile::active()` latches on first use. Without it the client
            // wrote to `instances/default` while a shell-started one wrote to
            // `profiles/<name>`, and signing in through one and restarting
            // through the other looked exactly like the session being lost.
            "--profile" => {
                let name = args.next().ok_or("--profile needs a name")?;
                let dir = cordial_runtime::profile::dir(&name)?;
                cordial_runtime::profile::set_active(dir)?;
            }
            "--read-asset" => {
                opt.read_asset = Some(args.next().ok_or("--read-asset needs a name")?)
            }
            "--flag-overrides" => {
                let p = args.next().ok_or("--flag-overrides needs a path")?;
                opt.flag_overrides = Some(
                    std::fs::read_to_string(&p).map_err(|e| format!("{p}: {e}"))?,
                );
            }
            "--client-settings" => {
                opt.client_settings =
                    Some(args.next().ok_or("--client-settings needs a path")?)
            }
            "--gl-probe" => opt.gl_probe = true,
            "--window" => {
                let v = args.next().ok_or("--window needs a duration in seconds")?;
                opt.window_seconds = Some(v.parse().map_err(|_| "--window wants a number")?);
            }
            "--host-libc" => opt.host_libc = true,
            "--jni-onload" => opt.jni_onload = true,
            "--run" => {
                let v = args.next().ok_or("--run needs a duration in seconds")?;
                opt.run_seconds = v.parse().map_err(|_| "--run wants a number")?;
            }
            "--game-activity" => {
                opt.jni_onload = true;
                opt.game_activity = true;
            }
            // The URL a browser click produced, forwarded by the shell. It is
            // validated here, at the edge, rather than anywhere further in:
            // this is the process boundary the value crosses, and a bad one
            // should end the launch with a sentence rather than travel.
            "--join-url" => {
                let raw = args.next().ok_or("--join-url needs a URL")?;
                opt.join_url = Some(cordial_runtime::deeplink::validate(&raw)?);
            }
            "--dump-classes" => {
                opt.jni_onload = true;
                opt.dump_classes = Some(args.next().ok_or("--dump-classes needs a path")?);
            }
            "-v" | "--verbose" => opt.verbose = true,
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unrecognised argument: {other}")),
        }
    }
    if opt.lib_dir.is_empty() {
        return Err("--lib-dir is required".into());
    }
    Ok(opt)
}

/// The directory the engine should treat as its asset folder.
///
/// Not the APK. The engine's HTTP stack is curl, and curl's `CURLOPT_CAINFO`
/// needs a real filesystem path for `assets/ssl/cacert.pem`; handing it the
/// `.apk` names a file inside a file. So the APK's assets are unpacked once to
/// a cache directory and that is what `assetFolderPath` points at.
///
/// It points at the `content` **subdirectory**, not the unpack root. The
/// Waydroid capture is explicit about this:
///
/// ```text
/// [FLog::Output] setAssetFolder      /data/user/0/com.roblox.client/app_assets/content
/// [FLog::Output] setExtraAssetFolder /data/user/0/com.roblox.client/app_assets/ExtraContent
/// ```
///
/// The engine echoes back exactly the path it is given, and resolves its
/// siblings — `android/`, `ssl/`, `fonts/` — relative to the *parent*. Passing
/// the unpack root therefore sends every one of those lookups a level too high.
/// Cordial did that, and the engine's own log named the consequence:
///
/// ```text
/// [FLog::CreatorError] Error: boost::filesystem::canonical:
///     No such file or directory: ".../.cache/cordial/android"
/// ```
///
/// That throw aborts `SingleSurfaceApp` initialisation before it reaches
/// `setStage: (stage:Native)` and before it instantiates its controllers, which
/// is why `initializeLuaAppWithLoggedInUser` then ran at `(stage:None)` and
/// dereferenced a controller that was never built.
///
/// Falls back to the APK path if extraction fails, which keeps the old
/// behaviour rather than refusing to start over an asset folder — the loader
/// and asset paths still work without it.
fn asset_folder(apk: &Option<String>) -> String {
    let Some(apk) = apk else { return String::new() };
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
        .join("cordial/assets");
    match cordial_runtime::android::asset::extract_to(&base) {
        Ok(dir) => dir.join("content").to_string_lossy().into_owned(),
        Err(e) => {
            println!("  asset extraction failed ({e}); using the APK path");
            apk.clone()
        }
    }
}

/// The directory the engine runs *in*, and why it needs one of its own.
///
/// Roblox builds several paths from a root it was never given and resolves them
/// against the working directory: `./exe/cacert.pem`, `http/`, `sounds/`,
/// `cache/` and a `ContentProvider_<pid>` per launch. Two consequences, both
/// real:
///
/// * curl is handed `./exe/cacert.pem` as its trust store, does not find it, and
///   every HTTPS request fails — `error adding trust anchors from file`. The CA
///   bundle exists; it ships in the APK at `assets/ssl/cacert.pem`.
/// * whatever directory you launched from fills up with the engine's scratch
///   files. Running from a checkout littered this repository.
///
/// An Android app's working directory is its own sandbox, so giving the process
/// one is the faithful behaviour rather than a workaround. `--lib-dir` and
/// `--apk` are made absolute first, because they are the caller's paths and are
/// allowed to be relative to the caller's directory.
///
/// Never fatal: a client that starts in the wrong directory is more useful than
/// one that refuses to start.
fn enter_run_dir(opt: &mut Options) {
    for p in [&mut opt.lib_dir] {
        if let Ok(abs) = std::fs::canonicalize(&*p) {
            *p = abs.to_string_lossy().into_owned();
        }
    }
    if let Some(apk) = opt.apk.as_mut() {
        if let Ok(abs) = std::fs::canonicalize(&*apk) {
            *apk = abs.to_string_lossy().into_owned();
        }
    }

    // The engine's working directory, inside whichever profile this instance was
    // given. This used to compute `instances/default` by hand while the rest of
    // the process had moved to `profiles/<name>`, which put the run directory and
    // the data directory in different trees.
    let root = cordial_runtime::profile::active().join("run");
    if let Err(e) = std::fs::create_dir_all(root.join("exe")) {
        println!("  could not create {}: {e}", root.display());
        return;
    }

    // The trust store, from the APK's own copy. Linked rather than copied so a
    // re-extracted bundle is picked up without a stale duplicate.
    let ca = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
        .join("cordial/assets/ssl/cacert.pem");
    let dest = root.join("exe/cacert.pem");
    if ca.exists() && std::fs::read_link(&dest).ok().as_deref() != Some(ca.as_path()) {
        let _ = std::fs::remove_file(&dest);
        let _ = std::os::unix::fs::symlink(&ca, &dest);
    }

    if let Err(e) = std::env::set_current_dir(&root) {
        println!("  could not enter {}: {e}", root.display());
    }
}

/// The render resolution, and why it is not simply the window size.
///
/// Roblox sizes its framebuffers and picks UI asset resolutions from what the
/// surface reports, so 1280x720 is not just a small window — it is the whole
/// pipeline running at 720p. On a 1920x1200 panel that is the difference
/// between a native image and an upscaled one.
///
/// `CORDIAL_RESOLUTION=<w>x<h>`; defaults to 1280x720, and `CORDIAL_FULLSCREEN`
/// overrides both with the monitor's own size.
fn requested_resolution() -> (u32, u32) {
    let Ok(v) = std::env::var("CORDIAL_RESOLUTION") else {
        return (1280, 720);
    };
    let mut parts = v.split(['x', 'X']).map(str::trim);
    if let (Some(Ok(w)), Some(Ok(h))) = (
        parts.next().map(str::parse::<u32>),
        parts.next().map(str::parse::<u32>),
    ) {
        if w >= 320 && h >= 240 && w <= 7680 && h <= 4320 {
            return (w, h);
        }
    }
    println!("  CORDIAL_RESOLUTION={v:?} is not <w>x<h> within reason; using 1280x720");
    (1280, 720)
}

fn main() -> ExitCode {
    let mut opt = match parse() {
        Ok(o) => o,
        Err(msg) => {
            if !msg.is_empty() {
                eprintln!("error: {msg}\n");
            }
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };

    // Before anything this profile might do reaches a network, including the
    // client-settings fetch further down -- which is a real HTTP request over
    // `ureq`, made by Cordial itself, and would otherwise go out whatever
    // route this instance happens to have. `--profile` (or its absence) has
    // just been resolved by `parse()`, above, so `profile::active()` is
    // settled and this is the earliest point this can be checked.
    //
    // This duplicates the same call `cordial-shell`'s `launch.rs` makes
    // before it ever spawns this process -- deliberately, not by accident.
    // AGENTS.md documents running `cordial-run` directly, without the shell,
    // as a fully supported path (`cargo run --release --bin cordial-run --
    // ...`), and a gate that only lived in the shell would be a `vpn-required`
    // profile that silently stopped meaning anything the moment somebody
    // started the client the other documented way. See
    // `cordial_shell::network`'s own doc for what this does and does not
    // guarantee.
    if let Err(refusal) =
        cordial_shell::network::ensure_launchable(&cordial_runtime::profile::active())
    {
        eprintln!("error: {refusal}");
        return ExitCode::FAILURE;
    }

    // Which backend, and who asked for it, before the engine has had a chance to
    // `dlopen` anything. Said out loud on every run: the questions it answers are
    // "why is this slow" and "why does this look different from yesterday", and
    // those get asked from a support thread rather than from a terminal somebody
    // is willing to re-run with a trace variable set.
    cordial_runtime::graphics::report();

    // Before anything can resolve a path: Android's `/system`, served from a
    // directory Cordial builds out of the host's fonts. Roblox asks for
    // `/system/fonts/NotoSansCJK-Regular.ttc` during app startup and turns the
    // miss into an empty path and an unhandled exception.
    cordial_runtime::android::system::install();

    if let Some(apk) = &opt.apk {
        match cordial_runtime::android::asset::set_apk(std::path::Path::new(apk)) {
            Ok(()) => println!("assets: {apk}"),
            Err(e) => {
                eprintln!("bad --apk: {e}");
                return ExitCode::FAILURE;
            }
        }
    }

    // After the APK is registered (so the CA bundle can be extracted) and before
    // anything asks the engine to resolve a path.
    if opt.apk.is_some() {
        let _ = asset_folder(&opt.apk);
    }
    enter_run_dir(&mut opt);

    if let Some(name) = &opt.read_asset {
        match cordial_runtime::android::asset::probe(name) {
            Ok(len) => println!("asset {name}: {len} bytes"),
            Err(e) => {
                eprintln!("asset {name}: {e}");
                return ExitCode::FAILURE;
            }
        }
    }

    cordial_runtime::android::set_trace(std::env::var_os("CORDIAL_ANDROID_TRACE").is_some());

    // Who is signed in, before anything can ask.
    //
    // This is deliberately the earliest thing after the profile is settled, and
    // it can be: unlike the cookie restore below, it calls into no engine
    // symbol at all. `NativeUserJavaInterface` and `StartAppParams` live in
    // Cordial's own framework layer, so there is nothing to be too early for —
    // whereas being *late* is a real failure with two shapes, because
    // `StartAppParams` copies four of these fields once inside
    // `nativeAppBridgeV2StartAppWithParams` and the engine can query the other
    // mirror at any moment before that.
    //
    // The cookie alone was never enough: with a real session restored and the
    // engine confirmed holding it, `PlatformAccountRouter` still routed to
    // Landing, because it asks these mirrors and they said user 0. See
    // `cordial_runtime::identity` and docs/design/sign-in.md §9.
    cordial_runtime::identity::listen();
    cordial_runtime::identity::restore();

    // Started this early, before `JNI_OnLoad`, so the AT-SPI bus connection
    // (a D-Bus round trip) has as much time as possible to finish before the
    // engine's first `AccessibilityManager.isEnabled()` check — the whole
    // point of `native/accessibility.cpp` reading a plain atomic there rather
    // than blocking on D-Bus is wasted if this is started too late for the
    // atomic to have flipped by the time it matters. Not a hard ordering
    // guarantee (the bridge thread and the engine's own load sequence race),
    // but every millisecond of head start narrows that race rather than
    // widening it.
    cordial_runtime::android::accessibility::start();

    // Before the engine loads, so the governor is already up when the shader
    // compiles and the asset cache warms — the part of a launch most obviously
    // bound by a CPU that has not been asked to hurry yet.
    gamemode::register();

    let table = symtab::build(opt.host_libc);
    let totals = table.totals();

    println!(
        "symbol table: {} cordial, {} host, {} stub, {} total across {} libraries",
        totals.cordial,
        totals.host,
        totals.stub,
        totals.cordial + totals.host + totals.stub,
        table.libraries.len()
    );
    for (lib, s) in &table.stats {
        println!(
            "  {lib:<20} cordial={:<4} host={:<5} stub={}",
            s.cordial, s.host, s.stub
        );
    }
    for missing in &table.missing_host_libs {
        println!("  warning: host {missing} unavailable; its symbols are stubbed");
    }

    if opt.verbose {
        for (lib, entries) in &table.libraries {
            for e in entries {
                println!(
                    "  {lib:<20} {:<44} {}",
                    e.symbol,
                    e.source.label()
                );
            }
        }
    }

    if let Some(secs) = opt.window_seconds {
        match cordial_runtime::android::gl::probe_window(&table, secs) {
            Ok(r) => {
                println!("\nGL probe rendered into a real window (this is a test pattern, not Roblox):");
                println!("  renderer  {}", r.renderer);
                println!("  version   {}", r.version);
                println!("  readback  {:02x?}", r.pixel);
            }
            Err(e) => {
                eprintln!("\nwindow render failed: {e}");
                return ExitCode::FAILURE;
            }
        }
    }

    if opt.gl_probe {
        match cordial_runtime::android::gl::probe(&table) {
            Ok(r) => {
                println!("\nGLES2 context is live:");
                println!("  vendor    {}", r.vendor);
                println!("  renderer  {}", r.renderer);
                println!("  version   {}", r.version);
                println!("  readback  {:02x?} — drew and read it back", r.pixel);
            }
            Err(e) => {
                eprintln!("\nGL probe failed: {e}");
                return ExitCode::FAILURE;
            }
        }
    }

    println!("\ninitialising bionic linker...");
    linker::init();

    for (name, entries) in &table.libraries {
        let symbols: Vec<(String, *mut std::ffi::c_void)> = entries
            .iter()
            .map(|e| (e.symbol.to_string(), e.address))
            .collect();
        if let Err(e) = linker::register(name, &symbols) {
            eprintln!("failed to register {name}: {e}");
            return ExitCode::FAILURE;
        }
    }
    println!("registered {} virtual libraries", table.libraries.len());

    if let Err(e) = linker::set_library_path(&opt.lib_dir) {
        eprintln!("bad --lib-dir: {e}");
        return ExitCode::FAILURE;
    }
    println!("search path: {}", opt.lib_dir);

    println!("\nloading {} ...", opt.library);

    let start = Instant::now();
    let result = linker::dlopen(&opt.library, linker::RTLD_NOW);
    let elapsed = start.elapsed();

    let lib = match result {
        Ok(lib) => lib,
        Err(e) => {
            eprintln!("\nLOAD FAILED after {:.0?}: {e}", elapsed);
            stubs::report();
            return ExitCode::FAILURE;
        }
    };

    let (code_base, code_size) = lib.code_region();
    println!("\nLOADED in {:.0?}", elapsed);
    println!("  base       {:#x}", lib.base());
    println!(
        "  code       {code_base:#x} + {code_size} bytes ({:.1} MB)",
        code_size as f64 / (1024.0 * 1024.0)
    );

    match lib.symbol("JNI_OnLoad") {
        Some(p) => println!("  JNI_OnLoad {p:p}"),
        None => println!("  JNI_OnLoad not found"),
    }

    if opt.jni_onload {
        if let Some(p) = lib.symbol("JNI_OnLoad") {
            let Some(vm) = linker::jni::create_vm() else {
                eprintln!("could not create a JavaVM");
                return ExitCode::FAILURE;
            };
            println!("\nJavaVM at {vm:p}; calling JNI_OnLoad");

            match linker::jni::call_on_load(p) {
                // JNI versions are 0x000M_000m; 0x00010006 is JNI_VERSION_1_6.
                Ok(rc) => {
                    println!("JNI_OnLoad returned {rc:#x} = JNI {}.{}", rc >> 16, rc & 0xffff);
                }
                Err(e) => {
                    println!("JNI_OnLoad failed: {e}");
                    println!(
                        "\n  Roblox expects the Android bring-up sequence, not a bare JNI_OnLoad:\n                           a JavaVM, then GameActivity.initializeNativeCode called from Java with a\n                           real Activity. See docs/framework-api-inventory.md §3.3."
                    );
                }
            }

            if opt.game_activity {
                let skip_agdk = std::env::var_os("CORDIAL_SKIP_AGDK").is_some();
                let native = if skip_agdk {
                    // ActivityNativeMain, the manifest's real launch target, does
                    // not extend GameActivity. Driving both bring-ups at once
                    // creates AGDK's game thread, which then reads app-bridge
                    // state that only Cordial's calling thread ever touched.
                    println!("\nskipping AGDK; driving the app bridge alone");
                    None
                } else {
                    lib.symbol(
                        "Java_com_google_androidgamesdk_GameActivity_initializeNativeCode",
                    )
                };
                if skip_agdk {
                    // The bridge sequence, without a handle and without AGDK.
                    let (rw, rh) = requested_resolution();
                    match cordial_runtime::android::open_window(
                        rw, rh, &cordial_shell::host_window::title(),
                    ) {
                        Err(e) => println!("  no window: {e}"),
                        Ok(w) => {
                            let (width, height, _) = w.geometry();
                            cordial_runtime::android::config::set_screen(width, height);
                            let apk_path = asset_folder(&opt.apk);
                            // Order taken from a Waydroid capture of the real
                            // Android client (docs/traces/render-bringup-sequence.log),
                            // which logs:
                            //   nativeAppBridgeAppStart
                            //   nativeAppBridgeV2Init
                            //   nativeAppBridgeStartLuaAppDM
                            //   nativeAppBridgeV2StartApp
                            // StartLuaAppDM comes BEFORE StartApp. An earlier
                            // experiment here swapped them the other way on the
                            // strength of a crash appearing to move; the capture
                            // says that was backwards.
                            //
                            // Superseded note: the engine spawns its
                            // own 'Main' thread inside nativeGameGlobalInit,
                            // which independently races through the same
                            // StartLuaAppDM machinery our own explicit call
                            // drives. Calling StartAppWithParams — which
                            // delivers the surface — *before* StartLuaAppDM
                            // lets that background thread's own progress get
                            // substantially further (from dying during
                            // InitParams reflection to dying during
                            // StartAppParams/surface reflection) before it
                            // still crashes. Skipping our own StartLuaAppDM
                            // call entirely changes nothing, since the engine
                            // calls it on that background thread regardless
                            // — so it stays here, last, for parity with the
                            // engine's own onCreate order, but is provably
                            // redundant for this particular crash.
                            for (name, run) in [
                                ("nativeGameGlobalInit", 0),
                                ("nativeUpdateAdapterInit", 0),
                                ("nativeAppBridgeV2InitWithParams", 1),
                                ("nativeAppBridgeStartLuaAppDM", 0),
                                ("nativeAppBridgeV2StartAppWithParams", 2),
                            ] {
                                let sym = format!(
                                    "Java_com_roblox_engine_jni_NativeGLInterface_{name}"
                                );
                                let Some(f) = lib.symbol(&sym) else {
                                    println!("  {name} not exported");
                                    continue;
                                };
                                let r = match run {
                                    1 => linker::game_activity::appbridge_init(
                                        f, &apk_path, width, height,
                                    ),
                                    2 => linker::game_activity::appbridge_start_app(
                                        f, &apk_path, width, height,
                                    ),
                                    _ => linker::game_activity::appbridge_call_bare(f),
                                };
                                match r {
                                    Ok(()) => println!("  {name} ok"),
                                    Err(e) => println!("  {name} failed: {e}"),
                                }
                            }
                            println!("  pumping for {}s", opt.run_seconds);
                            // No AGDK handle on this path — it drives the app
                            // bridge directly and never calls
                            // initializeNativeCode, so onTouchEventNative etc.
                            // are never registered to deliver input to.
                            cordial_runtime::android::looper::pump(
                                std::time::Duration::from_secs(opt.run_seconds),
                                None,
                            );
                        }
                    }
                }

                match native {
                    None if !skip_agdk => eprintln!("  initializeNativeCode is not exported"),
                    None => {}
                    Some(f) => {
                        // `initStorageManagerNativeV3` takes *two different*
                        // directories. The Waydroid capture shows the real
                        // client using `<app>/files` and `<app>/cache`, with the
                        // engine putting `cache/flag_cache.dat` and
                        // `cache/tombstone.dat` under the second one. Cordial was
                        // passing a single path twice — and one that had never
                        // been created, since nothing here calls `mkdir`. An
                        // Android app's `files` and `cache` dirs always exist by
                        // the time any app code runs, so the engine is entitled
                        // to assume it.
                        // `profile::active()` rather than a second hand-rolled
                        // path: this used to compute `instances/default` here
                        // while everything else in the process had moved to
                        // `profiles/<name>`, so the engine's own storage ended up
                        // in a directory nothing else looked at.
                        let root = std::env::var("CORDIAL_FILES_DIR").unwrap_or_else(|_| {
                            format!("{}/data", cordial_runtime::profile::active().display())
                        });
                        let files = format!("{root}/files");
                        let cache = format!("{root}/cache");
                        for d in [&files, &cache] {
                            if let Err(e) = std::fs::create_dir_all(d) {
                                println!("  could not create {d}: {e}");
                            }
                        }
                        // Android's framework prepares the UI thread's looper
                        // before any app code runs, and AGDK's
                        // initializeNativeCode bails out with a zero handle if
                        // ALooper_forThread returns null. Nothing else prepares
                        // one here.
                        if !cordial_runtime::android::looper::prepare_for_current_thread() {
                            eprintln!("  could not prepare a looper for this thread");
                            return ExitCode::FAILURE;
                        }

                        // Client settings before initializeNativeCode.
                        // The engine's flags verdict is reported from a thread
                        // that initializeNativeCode starts, and it was arriving
                        // before any later delivery could possibly matter --
                        // every ordering tried downstream of this point still
                        // lost the race, because the decision had already been
                        // made. This is the last position that is actually
                        // earlier than the decision.
                        if let Some(f) = lib.symbol(
                            "Java_com_roblox_engine_jni_NativeGLInterface_nativeInitClientSettings",
                        ) {
                            let settings = cordial_runtime::client_settings::load(
                                opt.client_settings.as_deref(),
                            )
                            .unwrap_or_default();
                            match linker::game_activity::init_client_settings(
                                f, &settings, "", "",
                            ) {
                                Ok(code) => {
                                    println!("  early client settings ({} bytes) -> {code}", settings.len())
                                }
                                Err(e) => println!("  early client settings failed: {e}"),
                            }
                        }

                        println!("\ncalling GameActivity.initializeNativeCode");
                        match linker::game_activity::initialize(f, &files, &files, &files) {
                            Ok(handle) => {
                                println!("  native handle {handle:#x}");

                                // The engine renders into an ANativeWindow, so
                                // there has to be a real one before the surface
                                // callbacks arrive.
                                let (rw, rh) = requested_resolution();
                                match cordial_runtime::android::open_window(
                                    rw, rh, &cordial_shell::host_window::title(),
                                ) {
                                    Err(e) => println!("  no window: {e}"),
                                    Ok(w) => {
                                        let (width, height, format) = w.geometry();
                                        cordial_runtime::android::config::set_screen(width, height);
                                        println!("  window {width}x{height}");
                                        cordial_runtime::android::config::set_screen(width, height);

                                        // The engine's own init sequence, in the
                                        // order MainGameActivity.onCreate runs it.
                                        // Without the asset manager the engine
                                        // cannot read its own content, which is
                                        // why nothing downstream ever starts —
                                        // no app shell, no network, no frame.
                                        // `NativeSettingsInterface` — where the
                                        // app tells the engine which directories
                                        // it owns. Nothing here called these, so
                                        // the engine resolved every path it built
                                        // from them against the working
                                        // directory: `./appData`, `cache`,
                                        // `http`, `sounds`. The capture shows the
                                        // real client using absolute paths under
                                        // its own storage for all of them.
                                        // Signatures read out of the shipping
                                        // APK's dex.
                                        const SETTINGS: &str =
                                            "com/roblox/engine/jni/NativeSettingsInterface";
                                        let external = format!("{root}/external");
                                        let _ = std::fs::create_dir_all(&external);
                                        let dirs: &[(&str, Vec<&str>)] = &[
                                            (
                                                "Java_com_roblox_engine_jni_NativeSettingsInterface_nativeSetFilesDirectory",
                                                vec![files.as_str()],
                                            ),
                                            (
                                                "Java_com_roblox_engine_jni_NativeSettingsInterface_nativeSetCacheDirectory",
                                                vec![cache.as_str()],
                                            ),
                                            (
                                                "Java_com_roblox_engine_jni_NativeSettingsInterface_nativeSetExternalDirectory",
                                                vec![external.as_str()],
                                            ),
                                            (
                                                "Java_com_roblox_engine_jni_NativeSettingsInterface_nativeSetBaseDataDirectories",
                                                vec![files.as_str(), cache.as_str()],
                                            ),
                                        ];
                                        let assets_now = asset_folder(&opt.apk);
                                        let dirs2: &[(&str, &str, Vec<&str>)] = &[
                                            (
                                                "Java_com_roblox_client_startup_MainGameActivity_nativeSetAssetPath",
                                                "com/roblox/client/startup/MainGameActivity",
                                                vec![assets_now.as_str()],
                                            ),
                                            (
                                                "Java_com_roblox_engine_jni_NativeSettingsInterface_nativeSetRobloxVersion",
                                                SETTINGS,
                                                // The version the engine stamps on
                                                // its own log file names, so it is
                                                // the engine's own answer rather
                                                // than a guess.
                                                vec!["2.732.0.1043"],
                                            ),
                                            (
                                                "Java_com_roblox_engine_jni_NativeSettingsInterface_nativeSetRobloxChannel",
                                                SETTINGS,
                                                // The capture reports
                                                // `Build = googleProdRelease`; the
                                                // live channel is the empty one.
                                                vec![""],
                                            ),
                                        ];
                                        if let Some(f) = lib.symbol(
                                            "Java_com_roblox_engine_jni_NativeSettingsInterface_nativeSetDeviceInfo",
                                        ) {
                                            match linker::game_activity::set_device_info(
                                                f, width, height,
                                            ) {
                                                Ok(()) => println!("  device info set"),
                                                Err(e) => println!("  nativeSetDeviceInfo failed: {e}"),
                                            }
                                        }

                                        for (name, cls, args) in dirs2 {
                                            match lib.symbol(name) {
                                                None => println!("  {name} not exported"),
                                                Some(f) => match linker::game_activity::call_static_strings(
                                                    f, cls, args,
                                                ) {
                                                    Ok(()) => println!(
                                                        "  {} ok",
                                                        name.rsplit('_').next().unwrap_or(name)
                                                    ),
                                                    Err(e) => println!("  {name} failed: {e}"),
                                                },
                                            }
                                        }

                                        for (name, args) in dirs {
                                            match lib.symbol(name) {
                                                None => println!("  {name} not exported"),
                                                Some(f) => match linker::game_activity::call_static_strings(
                                                    f, SETTINGS, args,
                                                ) {
                                                    Ok(()) => println!(
                                                        "  {} ok",
                                                        name.rsplit('_').next().unwrap_or(name)
                                                    ),
                                                    Err(e) => println!("  {name} failed: {e}"),
                                                },
                                            }
                                        }

                                        // The cookie natives, resolved here
                                        // and used later.
                                        //
                                        // The engine keeps its cookie jar in
                                        // memory only — measured, not assumed:
                                        // a full `CORDIAL_TRACE_PATHS=1`
                                        // inventory of every file it opens has
                                        // no cookie jar in it. On Android the
                                        // Java side persists them and hands
                                        // them back at startup, and Cordial has
                                        // no Java side, which is the whole of
                                        // why signing in and restarting
                                        // presented as being logged out.
                                        //
                                        // The handler is registered *here*, as
                                        // early as it resolves, because it only
                                        // reports changes: one registered after
                                        // a `Set-Cookie` has already been dealt
                                        // with never hears about that cookie.
                                        // Restoring has to wait, and the call
                                        // that does it sits after the app
                                        // bridge with the measurement that put
                                        // it there.
                                        if cordial_runtime::cookies::enabled() {
                                            match lib.symbol(
                                                "Java_com_roblox_engine_jni_NativeSettingsInterface_nativeSetMultipleCookies",
                                            ) {
                                                None => println!(
                                                    "  [cookies] nativeSetMultipleCookies not exported; a saved session cannot be restored"
                                                ),
                                                // SAFETY: the symbol resolved
                                                // under its own name, so it is
                                                // the static native this
                                                // signature describes.
                                                Some(f) => unsafe {
                                                    cordial_runtime::cookies::set_push(f)
                                                },
                                            }
                                            match lib.symbol(
                                                "Java_com_roblox_engine_jni_NativeSettingsInterface_nativeGetCookiesForDomain",
                                            ) {
                                                None => println!(
                                                    "  [cookies] nativeGetCookiesForDomain not exported; a session cannot be saved"
                                                ),
                                                // SAFETY: as above.
                                                Some(f) => unsafe {
                                                    cordial_runtime::cookies::set_pull(f)
                                                },
                                            }

                                            // The engine's own notification that
                                            // its jar changed. Verified firing
                                            // four times in the Waydroid capture
                                            // on a logged-out start — the device
                                            // and tracking cookies exercise the
                                            // identical plumbing an auth cookie
                                            // does.
                                            match lib.symbol(
                                                "Java_com_roblox_universalapp_cookie_JNICookieProtocol_updateOnSetCookieHandler",
                                            ) {
                                                None => println!(
                                                    "  [cookies] updateOnSetCookieHandler not exported; cookie changes will not be noticed"
                                                ),
                                                Some(f) => match linker::game_activity::cookies_register_handler(
                                                    f,
                                                    cordial_runtime::cookies::observe_host,
                                                ) {
                                                    Ok(()) => println!("  [cookies] OnSetCookieHandler registered"),
                                                    Err(e) => println!("  [cookies] updateOnSetCookieHandler failed: {e}"),
                                                },
                                            }
                                        } else {
                                            println!("  [cookies] persistence off (CORDIAL_SKIP_COOKIES)");
                                        }

                                        let files = files.clone();
                                        let cache = cache.clone();
                                        let steps: Vec<(&str, Box<dyn Fn(*mut std::ffi::c_void) -> Result<(), String>>)> = vec![
                                            (
                                                "Java_com_roblox_client_JNIAAssetManagerSetup_initNative",
                                                Box::new(linker::game_activity::asset_manager_init),
                                            ),
                                            (
                                                "Java_com_roblox_client_LocalStorageManager_initStorageManagerNativeV3",
                                                Box::new(move |f| {
                                                    linker::game_activity::storage_init(f, &files, &cache)
                                                }),
                                            ),
                                        ];
                                        for (name, run) in steps {
                                            match lib.symbol(name) {
                                                None => println!("  {name} not exported"),
                                                Some(f) => match run(f) {
                                                    Ok(()) => println!(
                                                        "  {} ok",
                                                        name.rsplit('_').next().unwrap_or(name)
                                                    ),
                                                    Err(e) => println!("  {name} failed: {e}"),
                                                },
                                            }
                                        }
                                        if let Some(p) = lib.symbol(
                                            "Java_com_roblox_client_startup_MainGameActivity_nativeAppBridgeSetInitParams",
                                        ) {
                                            // `PlatformParams.assetFolderPath`,
                                            // which is the same field the app
                                            // bridge gets — so it takes the same
                                            // unpacked `content` directory, not
                                            // the `.apk`. Naming the archive here
                                            // made every path the engine built
                                            // from it a file inside a file.
                                            match linker::game_activity::set_init_params(
                                                p,
                                                &asset_folder(&opt.apk),
                                                width,
                                                height,
                                            ) {
                                                Ok(()) => println!("  init params set"),
                                                Err(e) => println!("  init params failed: {e}"),
                                            }
                                        }

                                        // Client settings BEFORE the flag
                                        // calls. The engine reports its flags
                                        // verdict once, early, and the first
                                        // "flags FAILED" was arriving before
                                        // nativeInitClientSettings had been
                                        // called at all -- the settings were
                                        // being delivered after the decision
                                        // they were supposed to inform.
                                        // The network counterpart, called the
                                        // way the real app calls it: on
                                        // Android, Cordial's role (the host
                                        // app) is to fetch client settings and
                                        // hand the response to the engine —
                                        // the engine does not fetch its own.
                                        // `--client-settings` supplies that
                                        // real response body; the other two
                                        // string arguments' roles were not
                                        // pinned down with confidence, so
                                        // empty strings are the honest
                                        // starting point. The `int` the
                                        // engine returns is logged directly —
                                        // it is a far more reliable signal
                                        // than the onFlagsFailed/onFlagsLoaded
                                        // print, which comes from an
                                        // unrelated async path.
                                        if let Some(f) = lib.symbol(
                                            "Java_com_roblox_engine_jni_NativeGLInterface_nativeInitClientSettings",
                                        ) {
                                            // Cordial is the host app, so
                                            // Cordial does the fetch the app
                                            // would do. Cached on disk, so a
                                            // repeat launch is not a repeat
                                            // request.
                                            let settings =
                                                cordial_runtime::client_settings::load(
                                                    opt.client_settings.as_deref(),
                                                )
                                                .unwrap_or_default();
                                            println!(
                                                "  client settings: {} bytes",
                                                settings.len()
                                            );
                                            // Which of the three strings is the
                                            // settings document is not
                                            // established — the descriptor is
                                            // (String,String,String)I and the
                                            // engine's only clue is a
                                            // "ParseFailure on overrides" log
                                            // string, so one of the others is
                                            // an overrides document. Selectable
                                            // rather than guessed, so the
                                            // question can be settled by
                                            // running it.
                                            let pos = std::env::var("CORDIAL_CS_POS")
                                                .ok()
                                                .and_then(|v| v.parse::<u8>().ok())
                                                .unwrap_or(0);
                                            let (a, b, c) = match pos {
                                                1 => ("", settings.as_str(), ""),
                                                2 => ("", "", settings.as_str()),
                                                // Established by experiment:
                                                // the document goes first, and
                                                // 0 comes back. See
                                                // client_settings.rs.
                                                _ => (settings.as_str(), "", ""),
                                            };
                                            match linker::game_activity::init_client_settings(
                                                f, a, b, c,
                                            ) {
                                                Ok(code) => println!(
                                                    "  nativeInitClientSettings -> {code}"
                                                ),
                                                Err(e) => println!(
                                                    "  nativeInitClientSettings failed: {e}"
                                                ),
                                            }
                                        }
                                        // NOT called by default: passing an
                                        // empty `ArrayList` here reproducibly
                                        // crashes synchronously, on this
                                        // thread, inside libc's `_IO_fflush`
                                        // (fault address 0x8 — a near-null
                                        // pointer a small struct offset in),
                                        // verified live under lldb. That is
                                        // worse than the pre-existing
                                        // asynchronous crash this session set
                                        // out to leave alone, so this call is
                                        // wired but disabled pending a real
                                        // list argument. See the report for
                                        // detail. It is unconditional now:
                                        // that crash was a CONSEQUENCE of the
                                        // settings not being accepted, and with
                                        // nativeInitClientSettings returning 0
                                        // this call succeeds.
                                            if let Some(f) = lib.symbol(
                                                "Java_com_roblox_engine_jni_NativeGLInterface_nativePostClientSettingsLoadedInitialization3",
                                            ) {
                                                match linker::game_activity::post_client_settings_loaded(f) {
                                                    Ok(()) => println!(
                                                        "  postClientSettingsLoadedInitialization3 ok"
                                                    ),
                                                    Err(e) => println!(
                                                        "  postClientSettingsLoadedInitialization3 failed: {e}"
                                                    ),
                                                }
                                            }

                                        // Flags before anything else asks for
                                        // them: bootstrapTheApp's whole job is to
                                        // reach this, and the engine reports
                                        // onFlagsFailed without it.
                                        if let Some(f) = lib.symbol(
                                            "Java_com_roblox_client_flags_FlagJniInterface_nativeInitializeNativeFlags",
                                        ) {
                                            // Flag NAMES, not the settings
                                            // document — the engine loads
                                            // values itself. Feeding the
                                            // document here was a bug once
                                            // already, so it is deliberately
                                            // NOT client_settings::load().
                                            //
                                            // The list is built in because the
                                            // real client always passes it: a
                                            // Waydroid capture of this APK logs
                                            // "flagCount = 139" and names each
                                            // one. See docs/traces/README.md.
                                            // An explicit --client-settings file
                                            // still overrides it, for
                                            // experimenting with other lists.
                                            const FLAG_NAMES: &str = include_str!(
                                                "../native-flag-names.txt"
                                            );
                                            let settings = opt
                                                .client_settings
                                                .as_deref()
                                                .and_then(|p| std::fs::read_to_string(p).ok())
                                                .unwrap_or_else(|| FLAG_NAMES.to_string());
                                            println!(
                                                "  flag names: {}",
                                                settings.lines().filter(|l| !l.trim().is_empty()).count()
                                            );
                                            match linker::game_activity::init_flags(f, &settings) {
                                                Ok(()) => println!("  flags initialised"),
                                                Err(e) => println!("  flag init failed: {e}"),
                                            }
                                        }

                                        // `--flag-overrides <f>`: JSON handed
                                        // straight through to
                                        // nativePreloadFlagOverrides, so
                                        // candidate payload shapes can be
                                        // compared against their effect on the
                                        // flags verdict and JNI trace. This was
                                        // previously parsed but never actually
                                        // wired to a call — the "no extra
                                        // logging" result recorded earlier in
                                        // docs/analysis/flag-init.md was
                                        // therefore not a real negative
                                        // result; nothing was ever invoked.
                                        if let Some(json) = opt.flag_overrides.as_deref() {
                                            // `opt.flag_overrides` already holds the
                                            // *file contents* (read at argument-parsing
                                            // time, below) — not a path. An earlier
                                            // version of this call re-read it as if it
                                            // were a path, which silently failed and
                                            // passed an empty string through; that is
                                            // almost certainly why the FLog-channel
                                            // experiment recorded in
                                            // docs/analysis/flag-init.md produced no
                                            // extra logging. Fixed here.
                                            if let Some(f) = lib.symbol(
                                                "Java_com_roblox_client_startup_MainGameActivity_nativePreloadFlagOverrides",
                                            ) {
                                                match linker::game_activity::preload_flag_overrides(
                                                    f, json,
                                                ) {
                                                    Ok(()) => println!(
                                                        "  flag overrides preloaded ({} bytes)",
                                                        json.len()
                                                    ),
                                                    Err(e) => println!(
                                                        "  nativePreloadFlagOverrides failed: {e}"
                                                    ),
                                                }
                                            } else {
                                                println!(
                                                    "  nativePreloadFlagOverrides not exported"
                                                );
                                            }
                                        }

                                        // The offline counterpart:
                                        // `readLocalFlags()` makes the engine
                                        // read whatever bundled/cached flag
                                        // defaults it has on disk, with no
                                        // network round trip and nothing
                                        // impersonating Roblox's servers.
                                        // Nothing on the `ActivityNativeMain`
                                        // chain calls this in the real app —
                                        // its only dex caller is a different
                                        // startup path — so it is otherwise
                                        // dead code here.
                                        if let Some(f) = lib.symbol(
                                            "Java_com_roblox_engine_jni_NativeGLInterface_readLocalFlags",
                                        ) {
                                            match linker::game_activity::read_local_flags(f) {
                                                Ok(()) => println!("  local flags read"),
                                                Err(e) => println!("  readLocalFlags failed: {e}"),
                                            }
                                        }


                                        // Kicks the engine's initialisation once
                                        // everything it depends on is in place.
                                        if let Some(f) = lib.symbol(
                                            "Java_com_roblox_client_startup_MainGameActivity_nativeRetryInit",
                                        ) {
                                            match linker::game_activity::call_bare(f) {
                                                Ok(()) => println!("  retryInit ok"),
                                                Err(e) => println!("  retryInit failed: {e}"),
                                            }
                                        }

                                        // Resolve the input path Roblox's own
                                        // interface reads. AGDK's
                                        // onTouchEventNative is accepted by the
                                        // engine and ignored by the Lua UI; this
                                        // is what actually moves anything.
                                        {
                                            let mv = lib.symbol(
                                                "Java_com_roblox_engine_jni_NativeInputInterface_nativePassMouseMove",
                                            ).unwrap_or(std::ptr::null_mut());
                                            let bt = lib.symbol(
                                                "Java_com_roblox_engine_jni_NativeInputInterface_nativePassMouseButton",
                                            ).unwrap_or(std::ptr::null_mut());
                                            let wh = lib.symbol(
                                                "Java_com_roblox_engine_jni_NativeInputInterface_nativePassMouseWheel",
                                            ).unwrap_or(std::ptr::null_mut());
                                            let ke = lib.symbol(
                                                "Java_com_roblox_engine_jni_NativeGLInterface_nativePassKeyEvent",
                                            ).unwrap_or(std::ptr::null_mut());
                                            let tx = lib.symbol(
                                                "Java_com_roblox_engine_jni_NativeGLInterface_nativePassText",
                                            ).unwrap_or(std::ptr::null_mut());
                                            let sy = lib.symbol(
                                                "Java_com_roblox_engine_jni_NativeGLInterface_syncTextboxTextAndCursorPosition2",
                                            ).unwrap_or(std::ptr::null_mut());
                                            let uk = lib.symbol(
                                                "Java_com_roblox_engine_jni_NativeGLInterface_updateKeyboardSize",
                                            ).unwrap_or(std::ptr::null_mut());
                                            cordial_runtime::android::input::set_input_natives(mv, bt, wh, ke, tx, sy, uk);
                                            if mv.is_null() || bt.is_null() {
                                                println!("  input: NativeInputInterface not fully exported; UI input will not work");
                                            }
                                            // Named separately from the pair
                                            // above, because a build that
                                            // exports move and button but not
                                            // wheel has a working pointer and a
                                            // dead scroll wheel — which is
                                            // exactly the report this line was
                                            // added chasing, and "UI input will
                                            // not work" would be the wrong
                                            // thing to print for it.
                                            println!(
                                                "  input: nativePassMouseWheel {}",
                                                if wh.is_null() { "NOT exported; the scroll wheel will do nothing" } else { "resolved" }
                                            );

                                            // The one native on this interface
                                            // Cordial reads rather than writes:
                                            // whether the engine wants the
                                            // pointer locked to the window
                                            // centre. Nothing had ever called
                                            // it, so a first-person camera had
                                            // no way to ask for the cursor.
                                            // See `input::engine_wants_pointer_lock`
                                            // for what is still INFERRED about
                                            // which direction it is meant to be
                                            // read in.
                                            let ml = lib.symbol(
                                                "Java_com_roblox_engine_jni_NativeInputInterface_nativeGetMainWindowIsMouseLockedCenter",
                                            ).unwrap_or(std::ptr::null_mut());
                                            cordial_runtime::android::input::set_mouse_lock_native(ml);
                                            println!(
                                                "  input: nativeGetMainWindowIsMouseLockedCenter {}",
                                                if ml.is_null() {
                                                    "NOT exported; pointer capture falls back to the mouse button alone"
                                                } else {
                                                    "resolved"
                                                }
                                            );
                                        }

                                        // A read-only probe of the engine's own
                                        // verdict on whether login is rendered
                                        // by the Lua app shell rather than a
                                        // WebView — the question that decides
                                        // whether an embedded browser is needed
                                        // at all. See docs/design/sign-in.md.
                                        //
                                        // Behind a switch because it is a tool
                                        // for whoever is working on sign-in, not
                                        // something every launch should print.
                                        // It calls an exported boolean native
                                        // and prints the answer; it drives no UI
                                        // and enters no credentials.
                                        if std::env::var_os("CORDIAL_SIGNIN_PROBE").is_some() {
                                            match lib.symbol(
                                                "Java_com_roblox_engine_jni_NativeSettingsInterface_nativeIsLuaLoginEnabled",
                                            ) {
                                                None => println!(
                                                    "  [sign-in] nativeIsLuaLoginEnabled not exported"
                                                ),
                                                Some(f) => match linker::game_activity::call_static_bare_bool(
                                                    f, SETTINGS,
                                                ) {
                                                    Ok(v) => println!(
                                                        "  [sign-in] nativeIsLuaLoginEnabled() -> {v}"
                                                    ),
                                                    Err(e) => println!(
                                                        "  [sign-in] nativeIsLuaLoginEnabled() failed: {e}"
                                                    ),
                                                },
                                            }
                                        }

                                        // Android's Application.ActivityLifecycleCallbacks
                                        // order. The engine stores per-Activity
                                        // context as these fire, and nothing was
                                        // driving them — which is why it held a
                                        // null JNIEnv on the game thread.
                                        {
                                            const PREFIX: &str =
                                                "Java_com_roblox_universalapp_activitylifecyclecallbacks_JNIActivityLifecycleCallbacks_";
                                            let activity = "com.roblox.client.ActivityNativeMain";
                                            let mut fired = 0;
                                            for stage in [
                                                "nativeOnPreCreated", "nativeOnCreated",
                                                "nativeOnPostCreated", "nativeOnPreStarted",
                                                "nativeOnStarted", "nativeOnPostStarted",
                                                "nativeOnPreResumed", "nativeOnResumed",
                                                "nativeOnPostResumed",
                                            ] {
                                                if let Some(f) =
                                                    lib.symbol(&format!("{PREFIX}{stage}"))
                                                {
                                                    match linker::game_activity::activity_lifecycle(
                                                        f, activity,
                                                    ) {
                                                        Ok(()) => fired += 1,
                                                        Err(e) => {
                                                            println!("  {stage} failed: {e}")
                                                        }
                                                    }
                                                }
                                            }
                                            println!("  activity lifecycle: {fired}/9 fired");
                                        }

                                        // Globals first. Disassembly of the
                                        // ActivityNativeMain chain gives this
                                        // order, and calling StartLuaAppDM
                                        // without them crashes on a null JNIEnv
                                        // the engine expects to have been stored
                                        // by the globals init.
                                        for name in [
                                            "Java_com_roblox_engine_jni_NativeGLInterface_nativeGameGlobalInit",
                                            "Java_com_roblox_engine_jni_NativeGLInterface_nativeUpdateAdapterInit",
                                        ] {
                                            match lib.symbol(name) {
                                                None => println!("  {name} not exported"),
                                                Some(f) => match linker::game_activity::appbridge_call_bare(f) {
                                                    Ok(()) => println!(
                                                        "  {} ok",
                                                        name.rsplit('_').next().unwrap_or(name)
                                                    ),
                                                    Err(e) => println!("  {name} failed: {e}"),
                                                },
                                            }
                                        }

                                        // The app bridge proper. ActivitySplash —
                                        // the only launcher Activity — defaults
                                        // to ActivityNativeMain, not the AGDK
                                        // MainGameActivity, and this is the chain
                                        // that actually brings the client up.
                                        let apk_path = asset_folder(&opt.apk);
                                        if let Some(f) = lib.symbol(
                                            "Java_com_roblox_engine_jni_NativeGLInterface_nativeAppBridgeV2InitWithParams",
                                        ) {
                                            match linker::game_activity::appbridge_init(
                                                f, &apk_path, width, height,
                                            ) {
                                                Ok(()) => println!("  app bridge initialised"),
                                                Err(e) => println!("  app bridge init failed: {e}"),
                                            }
                                        }

                                        // The saved session goes back in here,
                                        // and this position was measured rather
                                        // than chosen.
                                        //
                                        // docs/design/sign-in.md §5.2 said to
                                        // call `nativeSetMultipleCookies`
                                        // before `nativeAppBridgeSetInitParams`,
                                        // reasoning that the cookie must be in
                                        // place before the engine starts hitting
                                        // `authenticated/*`. The reasoning is
                                        // right and the position was wrong:
                                        // called that early the native returns
                                        // cleanly and does nothing at all.
                                        // `CORDIAL_COOKIE_PROBE=1` sets a marker
                                        // and reads it straight back at four
                                        // points in this sequence, and the
                                        // answer is 0 bytes at startup, 0 after
                                        // init params, and 51 from here onwards
                                        // — the engine's cookie jar does not
                                        // exist until `nativeAppBridgeV2InitWithParams`
                                        // has built it. That document has been
                                        // corrected.
                                        //
                                        // Still before `StartLuaAppDM` below,
                                        // which is what actually sets the app
                                        // shell running and produces the first
                                        // `authenticated/*` request, so the
                                        // ordering the design doc wanted is
                                        // preserved.
                                        if cordial_runtime::cookies::enabled() {
                                            match lib.symbol(
                                                "Java_com_roblox_engine_jni_NativeSettingsInterface_nativeSetMultipleCookies",
                                            ) {
                                                None => {}
                                                Some(f) => {
                                                    let n = cordial_runtime::cookies::restore(f);
                                                    println!(
                                                        "  [cookies] restored {n} domain(s) from {}",
                                                        cordial_runtime::cookies::where_kept()
                                                    );
                                                    // Whether the two natives
                                                    // agree on what a domain is,
                                                    // and whether they are up
                                                    // yet. Off by default: it
                                                    // puts a marker cookie in
                                                    // the engine's jar, which is
                                                    // fine for a diagnostic run
                                                    // and not for an ordinary
                                                    // launch.
                                                    if std::env::var_os("CORDIAL_COOKIE_PROBE").is_some() {
                                                        if let Some(g) = lib.symbol(
                                                            "Java_com_roblox_engine_jni_NativeSettingsInterface_nativeGetCookiesForDomain",
                                                        ) {
                                                            cordial_runtime::cookies::probe(f, g, "restore");
                                                        }
                                                    }
                                                }
                                            }
                                        }

                                        // And the engine's own copy of who is
                                        // signed in, which is a third place
                                        // and not a duplicate of the two
                                        // mirrors `identity::restore` fills.
                                        //
                                        // Measured, because filling the mirrors
                                        // in was not enough on its own:
                                        // `CORDIAL_TRACE_IDENTITY=1` shows the
                                        // engine asking all six of
                                        // `NativeUserJavaInterface`'s methods
                                        // four times each, being told a real
                                        // user every time, and still reaching
                                        // `app ready: Landing`. The mirrors are
                                        // what Cordial answers when asked; this
                                        // is what the engine keeps for itself.
                                        //
                                        // Here rather than earlier for the same
                                        // reason as the cookie restore above:
                                        // this class's natives return cleanly
                                        // and do nothing until
                                        // `nativeAppBridgeV2InitWithParams` has
                                        // built what they write into. Still
                                        // before `StartLuaAppDM`, which is what
                                        // starts the app shell that routes.
                                        if cordial_runtime::identity::enabled() {
                                            match lib.symbol(
                                                "Java_com_roblox_engine_jni_NativeSettingsInterface_nativeSetUserId",
                                            ) {
                                                None => println!(
                                                    "  [identity] nativeSetUserId not exported; the engine will not know who is signed in"
                                                ),
                                                Some(f) => {
                                                    if cordial_runtime::identity::push_user_id(f) {
                                                        println!("  [identity] the engine has been told which user is signed in");
                                                    }
                                                }
                                            }
                                        }

                                        // The deep link, if this launch is one.
                                        //
                                        // Here because these are the engine's
                                        // *cold start* URL natives and this is
                                        // the cold-start moment: after
                                        // `nativeAppBridgeV2InitWithParams`,
                                        // which is what builds the protocol
                                        // machinery they talk to — the same
                                        // ordering constraint the cookie
                                        // restore above is placed by — and
                                        // before `StartLuaAppDM`, which is
                                        // where `ActivityNativeMain` consults
                                        // `isColdStartDeeplinkToGame()` on
                                        // Android.
                                        if let Some(url) = &opt.join_url {
                                            let outcome =
                                                cordial_runtime::deeplink::deliver(lib, url);
                                            println!("[deeplink] outcome: {outcome:?}");
                                        }

                                        if std::env::var_os("CORDIAL_SKIP_LUA_DM").is_none() {
                                        if let Some(f) = lib.symbol(
                                            "Java_com_roblox_engine_jni_NativeGLInterface_nativeAppBridgeStartLuaAppDM",
                                        ) {
                                            match linker::game_activity::appbridge_call_bare(f) {
                                                Ok(()) => println!("  Lua app DataModel started"),
                                                Err(e) => println!("  StartLuaAppDM failed: {e}"),
                                            }
                                        }
                                        }

                                        // The capture puts this immediately
                                        // before nativeAppBridgeV2StartApp:
                                        //   setTaskSchedulerBackgroundMode()
                                        //     enable:false context:ASMA.start
                                        // A task scheduler still in background
                                        // mode is one that has been told not to
                                        // render.
                                        if let Some(f) = lib.symbol(
                                            "Java_com_roblox_engine_jni_NativeGLInterface_setTaskSchedulerBackgroundMode",
                                        ) {
                                            match linker::game_activity::call_static_bool_string(
                                                f,
                                                "com/roblox/engine/jni/NativeGLInterface",
                                                false,
                                                "ASMA.start",
                                            ) {
                                                Ok(()) => println!("  task scheduler foregrounded"),
                                                Err(e) => {
                                                    println!("  setTaskSchedulerBackgroundMode failed: {e}")
                                                }
                                            }
                                        }

                                        // And the call that delivers the surface.
                                        if let Some(f) = lib.symbol(
                                            "Java_com_roblox_engine_jni_NativeGLInterface_nativeAppBridgeV2StartAppWithParams",
                                        ) {
                                            match linker::game_activity::appbridge_start_app(
                                                f, &apk_path, width, height,
                                            ) {
                                                Ok(()) => println!("  app started with surface"),
                                                Err(e) => println!("  StartApp failed: {e}"),
                                            }
                                        }

                                        // The two `UpdateSurface...WithPlatformParams` calls,
                                        // here because this is where Sober makes them — at about
                                        // 3.79s, immediately after StartApp and before any join.
                                        //
                                        // Sober makes 87 `JNIAppBridge` calls in a session and
                                        // Cordial made 3; these were two of the missing ones, and
                                        // neither was referenced anywhere in this tree. Whether
                                        // they are what stops the server sending disconnect
                                        // reason 304 sixty-one seconds into a join is **not
                                        // established** — this is the largest measured difference
                                        // between a client that stays connected and one that does
                                        // not, which earns an experiment rather than a claim.
                                        //
                                        // `CORDIAL_SKIP_UPDATE_SURFACE=1` is the control: it
                                        // restores exactly the previous behaviour, so a session
                                        // that survives can be shown to survive *because* of
                                        // these rather than because something else moved.
                                        if std::env::var_os("CORDIAL_SKIP_UPDATE_SURFACE").is_none() {
                                            for (native, game) in [
                                                ("Java_com_roblox_engine_jni_NativeGLInterface_nativeAppBridgeV2UpdateSurfaceAppWithPlatformParams", false),
                                                ("Java_com_roblox_engine_jni_NativeGLInterface_nativeAppBridgeV2UpdateSurfaceGameWithPlatformParams", true),
                                            ] {
                                                let which = if game { "game" } else { "app" };
                                                match lib.symbol(native) {
                                                    Some(f) => match linker::game_activity::appbridge_update_surface(
                                                        f, &apk_path, width, height, game,
                                                    ) {
                                                        Ok(()) => println!("  surface+platform params delivered ({which})"),
                                                        Err(e) => println!("  UpdateSurface {which} failed: {e}"),
                                                    },
                                                    // Not a warning to swallow: the export list is
                                                    // per build, and a rename is exactly the kind
                                                    // of change that would make this quietly stop.
                                                    None => {
                                                        println!("  UpdateSurface {which}: the engine does not export it");
                                                        cordial_runtime::unimplemented::placeholder(
                                                            &format!("nativeAppBridgeV2UpdateSurface{which}WithPlatformParams"),
                                                            "not exported by this build; not called",
                                                        );
                                                    }
                                                }
                                            }
                                        }

                                        match linker::game_activity::start(
                                            handle, width, height, format,
                                        ) {
                                            Ok(()) => {
                                                println!("  surface handed to the engine");
                                                // `setInputConnectionNative`: on real
                                                // Android this is Java calling native
                                                // code from inside
                                                // `onCreateInputConnection`, which
                                                // Cordial has no view system to
                                                // trigger — driven directly, once,
                                                // here, so the engine has somewhere
                                                // to send `setState`/
                                                // `setSoftKeyboardActive`/
                                                // `restartInput` before it ever tries.
                                                // `Ok(None)` means the native was not
                                                // registered yet, the same
                                                // not-yet-vs-failed distinction the
                                                // other AGDK natives use.
                                                match linker::game_activity::set_input_connection(handle) {
                                                    Ok(Some(())) => println!("  InputConnection registered with the engine"),
                                                    Ok(None) => println!("  setInputConnectionNative not registered yet; IME state will not reach Cordial"),
                                                    Err(e) => println!("  setInputConnectionNative failed: {e}"),
                                                }
                                                let secs = opt.run_seconds;
                                                if secs == 0 {
                                                    println!(
                                                        "  pumping the looper until the window is closed (no --run timer)"
                                                    );
                                                } else {
                                                    println!("  pumping the looper for {secs}s");
                                                }
                                                // Android's UI thread runs the
                                                // message loop; AGDK put its
                                                // pipes on this thread's looper.
                                                // The same loop also drains
                                                // host mouse/keyboard input and
                                                // delivers it through this
                                                // handle's onTouchEventNative /
                                                // onKeyDownNative/UpNative.
                                                // Plugins run alongside the
                                                // client, in their own
                                                // processes. Started here
                                                // rather than earlier so they
                                                // observe a client that is
                                                // already up, and so a plugin
                                                // that misbehaves cannot
                                                // interfere with bring-up.
                                                let n = cordial_runtime::plugin_host::start_all();
                                                if n > 0 {
                                                    println!("  {n} plugin(s) running");
                                                }
                                                cordial_runtime::android::looper::pump(
                                                    std::time::Duration::from_secs(secs),
                                                    Some(handle),
                                                );
                                                if std::env::var_os("CORDIAL_COUNT_GL").is_some() {
                                                    // What each thread is blocked on. A game thread
                                                    // waiting on a socket, a futex, or nothing at all
                                                    // are three different problems.
                                                    println!("\n  threads:");
                                                    if let Ok(dir) = std::fs::read_dir("/proc/self/task") {
                                                        for e in dir.flatten() {
                                                            let p = e.path();
                                                            let name = std::fs::read_to_string(p.join("comm"))
                                                                .unwrap_or_default().trim().to_string();
                                                            let wchan = std::fs::read_to_string(p.join("wchan"))
                                                                .unwrap_or_default().trim().to_string();
                                                            let state = std::fs::read_to_string(p.join("stat"))
                                                                .ok()
                                                                .and_then(|s| s.rsplit(')').next()
                                                                    .and_then(|r| r.split_whitespace().next())
                                                                    .map(str::to_string))
                                                                .unwrap_or_default();
                                                            println!("    {name:<18} state={state:<2} wchan={wchan}");
                                                        }
                                                    }
                                                    println!(
                                                        "  looper polls: {}",
                                                        cordial_runtime::android::looper::POLLS
                                                            .load(std::sync::atomic::Ordering::Relaxed)
                                                    );
                                                    println!("\n  graphics calls Roblox made:");
                                                    for (name, n) in
                                                        cordial_runtime::android::glcount::report()
                                                    {
                                                        println!("    {name:<24} {n}");
                                                    }
                                                }
                                            }
                                            Err(e) => println!("  lifecycle failed: {e}"),
                                        }
                                    }
                                }
                            }
                            Err(e) => println!("  failed: {e}"),
                        }
                    }
                }
            }

            if let Some(path) = &opt.dump_classes {
                match linker::jni::dump_classes(path) {
                    Ok(()) => println!("  Java classes Roblox reached for -> {path}"),
                    Err(e) => eprintln!("  class dump failed: {e}"),
                }
            }
        }
    }

    stubs::report();

    // Everything the engine asked for that Cordial could not answer, in one
    // table: JNI classes and methods libjnivm never had, libc stubs, AGDK
    // natives called while unregistered, and framework calls that returned
    // something invented. Printed and written beside the engine's own logs,
    // because the question after a failure is "what did we fail to tell it"
    // and the answer used to be spread across four kinds of line.
    cordial_runtime::unimplemented::report();

    // Before `_exit`, which runs nothing. gamemoded would notice the process
    // was gone on its own — it reaps clients whose pid has vanished — but that
    // is a poll, so leaving it implicit means the governor stays raised for
    // however long the sweep takes after a session ends.
    gamemode::unregister();

    // Leave via _exit rather than returning.
    //
    // Roblox's static initialisers registered atexit handlers and DT_FINI_ARRAY
    // destructors that expect a live Android process — a JavaVM, a working
    // looper, its own stdio. Running them here segfaults during teardown, long
    // after the load this tool exists to verify has already succeeded. Clean
    // shutdown belongs with instance lifecycle in core; until then, reporting a
    // teardown crash as a load failure would be actively misleading.
    //
    // SAFETY: _exit is async-signal-safe and terminates without running any
    // handler. Nothing here owns a resource the kernel will not reclaim.
    unsafe { libc_exit(0) }
}

extern "C" {
    #[link_name = "_exit"]
    fn libc_exit(status: std::ffi::c_int) -> !;
}

/// Feral Interactive's GameMode, asked for over D-Bus.
///
/// GameMode is a request rather than a wrapper. There is nothing to link and
/// nothing to `LD_PRELOAD`: `gamemoded` owns `com.feralinteractive.GameMode` on
/// the session bus and takes `RegisterGame(i pid)` / `UnregisterGame(i pid)`.
/// While a client is registered it puts the CPU governor in performance, raises
/// the process's I/O and scheduling priority, puts the GPU in its performance
/// profile and inhibits the screensaver. That last one is not a footnote for a
/// game the user plays with a controller and does not touch the keyboard for.
///
/// **Absence is the ordinary case and must not fail a launch.** Most machines
/// do not have gamemoded, and this is an optimisation rather than a dependency
/// — a client that refused to start because a performance daemon was missing
/// would be a far worse bug than the frame it was trying to save. Every failure
/// here is reported in one line and stepped over.
///
/// On by default, which is what Sober does. `CORDIAL_GAMEMODE=0` turns it off,
/// and that is the control: it is the only way to show, in the same session,
/// that a timing difference came from this and not from something else.
mod gamemode {
    use std::sync::OnceLock;

    const SERVICE: &str = "com.feralinteractive.GameMode";
    const OBJECT: &str = "/com/feralinteractive/GameMode";

    /// Held for the life of the process rather than opened per call. Not because
    /// `RegisterGame` needs it — it registers a pid, and gamemoded watches that
    /// pid rather than this connection — but because [`unregister`] runs during
    /// teardown, and opening a bus connection is the wrong thing to be doing at
    /// the point where the engine's own destructors are already known to be
    /// unsafe to run.
    static CONNECTION: OnceLock<Option<zbus::blocking::Connection>> = OnceLock::new();

    /// Whether [`register`] actually got a yes, so [`unregister`] does not send
    /// an `UnregisterGame` for a registration that never happened.
    static REGISTERED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    fn enabled() -> bool {
        !matches!(
            std::env::var("CORDIAL_GAMEMODE").unwrap_or_default().trim(),
            "0" | "off" | "false" | "no"
        )
    }

    fn connection() -> Option<&'static zbus::blocking::Connection> {
        CONNECTION.get_or_init(|| zbus::blocking::Connection::session().ok()).as_ref()
    }

    /// `RegisterGame`/`UnregisterGame` both answer `0` for success and a
    /// negative number for a refusal, so the reply has to be read rather than
    /// just checked for not being a D-Bus error — gamemoded returns `-1` for a
    /// pid it will not accept and `-2` for one already registered, over a
    /// perfectly successful method call.
    fn call(method: &str) -> Result<i32, String> {
        let conn = connection().ok_or_else(|| "no session bus".to_string())?;
        let pid = std::process::id() as i32;
        let reply = conn
            .call_method(Some(SERVICE), OBJECT, Some(SERVICE), method, &(pid,))
            .map_err(|e| e.to_string())?;
        reply.body().deserialize::<i32>().map_err(|e| e.to_string())
    }

    pub fn register() {
        if !enabled() {
            println!("[gamemode] off (CORDIAL_GAMEMODE=0)");
            return;
        }
        match call("RegisterGame") {
            Ok(0) => {
                REGISTERED.store(true, std::sync::atomic::Ordering::Relaxed);
                println!(
                    "[gamemode] registered pid {}: performance governor, raised priority, \
                     GPU performance profile, screensaver inhibited",
                    std::process::id()
                );
            }
            // Said plainly rather than folded into the error path below. A
            // daemon that answered and declined is a different situation from
            // one that is not there, and only the second is the ordinary case.
            Ok(rc) => println!("[gamemode] gamemoded declined to register this process (rc {rc})"),
            Err(e) => println!("[gamemode] not available, continuing without it: {e}"),
        }
    }

    pub fn unregister() {
        if !REGISTERED.swap(false, std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        match call("UnregisterGame") {
            Ok(0) => println!("[gamemode] unregistered"),
            Ok(rc) => println!("[gamemode] UnregisterGame returned {rc}"),
            Err(e) => println!("[gamemode] UnregisterGame failed: {e}"),
        }
    }
}
