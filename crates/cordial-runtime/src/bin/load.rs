//! `cordial-load` — load `libroblox.so` with the bionic linker.
//!
//! This does not run Roblox. It proves the loader, the relocations and the TLS
//! layout work against the real 116 MB object, and turns
//! docs/framework-api-inventory.md into a prioritised list of what to implement.

use std::cell::Cell;
use std::process::ExitCode;
use std::rc::Rc;
use std::time::Instant;

use cordial_linker_sys as linker;
use cordial_runtime::{stubs, symtab};
// `ListModelExt` (`n_items`/`item`) and `Cast` (`downcast`), for
// `refresh_outputs` walking `gdk::Display::monitors()`. The same `gtk4` this
// crate already depends on for `instr_close_window` -- see that dependency's
// own comment in Cargo.toml for why a second gtk4-rs version is not an option.
use gtk4::prelude::*;

struct Options {
    lib_dir: String,
    library: String,
    apk: Option<String>,
    /// The `--profile` name, kept as well as resolved. `parse` hands the
    /// directory to `profile::set_active` immediately, but `main` has to claim
    /// the profile by name — see [`claim_profile`] — and re-deriving the name
    /// from the path afterwards is one more place for the two to disagree.
    profile: Option<String>,
    read_asset: Option<String>,
    check_overlays: bool,
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

  --lib-dir <dir>   directory holding the APK's lib/<abi>/ objects
  --library <name>  object to load (default: libroblox.so)
  --apk <path>      APK to serve assets from; without it AAssetManager_open fails
  --read-asset <p>  read one asset through the AAsset API and report its size
  --check-overlays  report which overlay files match nothing in this build, then exit
  --client-settings <f>  newline-free list of flag names to pre-cache.
                    NOT the ClientSettings document — the engine loads values itself
  --flag-overrides <f>  JSON passed to nativePreloadFlagOverrides. DIAGNOSTIC
                    ONLY: that native does nothing observable despite its name,
                    tested with several document shapes. To actually set a flag,
                    use ~/.config/cordial/flags.json (see CONTRIBUTING.md)
  --gl-probe        bring up GLES2 through the symbol table and read a pixel back
  --window <secs>   GL PROBE ONLY: open a window and draw a gradient for <secs>.
                    This is Cordial's own test pattern, not Roblox rendering.
  --profile <name>  which profile's storage, flags and plugin grants to run
                    against; the one named default when this is not given.
                    One client at a time per profile, held by a lock for the
                    life of the process (ADR-012); a second is refused rather
                    than allowed to write over the first one's Roblox storage
  --headless        run inside a nested `cage` compositor, so no window appears
                    on this session and nothing takes focus. For agents and CI.
                    Re-execs, because the compositor has to be the parent. Fails
                    rather than falling back to a visible window if cage is
                    missing. Pair with CORDIAL_DEV_CONTROL=1 or nothing can see
                    the client at all
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
                                     MAILBOX when the driver advertises it --
                                     responsive, and it costs power; 'fifo'
                                     saves the power and feels floatier),
                                     off (forward the engine's own choice, which
                                     is FIFO — this is the control for a frame
                                     rate measurement), or one of mailbox,
                                     immediate, uncapped, fifo, fifo-relaxed.
                                     'uncapped' means MAILBOX if the driver has
                                     it and IMMEDIATE otherwise. FIFO is the
                                     only mode the spec guarantees, so anything
                                     the driver does not advertise falls back to
                                     what the engine asked for. A plugin can ask
                                     for one through the CordialPresentMode flag
                                     key; this variable overrules it
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
        profile: None,
        read_asset: None,
        check_overlays: false,
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
                opt.profile = Some(name);
            }
            "--read-asset" => {
                opt.read_asset = Some(args.next().ok_or("--read-asset needs a name")?)
            }
            "--check-overlays" => opt.check_overlays = true,
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
            // Consumed by the re-exec in `main` long before this. Reaching
            // here means the handover did not happen, and silently ignoring it
            // would put a window on the screen of somebody who asked for none.
            "--headless" => {
                return Err(
                    "--headless should have been consumed before argument parsing; this \
                     is a bug in the re-exec, not in your command line"
                        .into(),
                )
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
                // Arms the join watchdog in `looper::pump`. Only a join Cordial
                // itself asked for is watched; one the user starts from inside
                // the app shell never reaches this parser, and a watchdog that
                // claimed to cover it would be reporting on something it cannot
                // see.
                cordial_runtime::android::looper::note_join_requested();
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
        Ok(dir) => {
            // The overlay resolver needs the *extraction root*, not the
            // `content` subdirectory handed to the engine: overlay names are
            // relative to `assets/`, so `content/…` is part of the name.
            // Both routes then share one index, which they must — an overlay
            // that applied to a texture reached through `AAssetManager` and
            // not to the same texture reached by path would be a bug nobody
            // would guess from the symptom (ADR-021).
            cordial_runtime::android::asset::set_asset_root(&dir);
            dir.join("content").to_string_lossy().into_owned()
        }
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
    // The development control socket, if this run asked for one. Started here
    // because `profile::active()` has latched by now, and the socket belongs
    // inside the profile so ADR-012's one-instance lock already covers it.
    cordial_runtime::devctl::start();

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
    let link_ca = |dest: std::path::PathBuf| {
        if !ca.exists() {
            return;
        }
        if let Some(parent) = dest.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if std::fs::read_link(&dest).ok().as_deref() != Some(ca.as_path()) {
            let _ = std::fs::remove_file(&dest);
            let _ = std::os::unix::fs::symlink(&ca, &dest);
        }
    };
    link_ca(root.join("exe/cacert.pem"));
    // And under the engine's *files* directory, because `exe/cacert.pem` is
    // relative to whatever root the engine has at the moment curl asks for it.
    // With `nativeSetFilesDirectory` called late that is the working directory
    // and the link above answers it; call the setter early and the engine builds
    // `<filesDir>/exe/cacert.pem` instead, finds nothing, and reports `error
    // adding trust anchors from file` on every HTTPS request -- which surfaces
    // as `fetch flag exception: HttpError: Unknown` and `getFlags: success =
    // false`, i.e. as a flags bug rather than a certificate one. Measured:
    // `CORDIAL_EARLY_DIRS=files` produces exactly that, and the control in the
    // same session does not. One symlink removes the dependency on which root
    // wins the race.
    let files_root = std::env::var("CORDIAL_FILES_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| cordial_runtime::profile::active().join("data"));
    link_ca(files_root.join("files/exe/cacert.pem"));

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
/// What `GameActivity.bootstrapTheApp()` runs, and whether it ran.
///
/// On Android this is Kotlin: the app fetches its client settings and its flag
/// set and hands both to the engine, and the engine calls it from inside
/// `initializeNativeCode`. Cordial is the host application, so this is Cordial's
/// job — it was simply being done in the wrong place. The delivery below used to
/// happen after `initializeNativeCode` returned, and a traced run shows the
/// engine calling `bootstrapTheApp`, getting an unresolved placeholder, and
/// reporting `gameActivity_onFlagsFailed` on the very next line. Nothing
/// delivered afterwards could have changed that verdict.
///
/// Function pointers rather than anything borrowed because the callback crosses
/// into C++ and back on the engine's own thread, with no lifetime to speak of.
struct BootstrapPlan {
    settings_native: usize,
    post_native: usize,
    flags_native: usize,
    /// `MainGameActivity.nativePreloadFlagOverrides(String)V`.
    ///
    /// Here rather than only at the diagnostic call site because the engine has
    /// a `getFlags: ParseFailure on preloaded overrides` error path, which means
    /// `getFlags` *consumes* preloaded overrides -- and Cordial has never
    /// supplied any on the bootstrap path. `docs/analysis/unresolved-java.md`
    /// reads the real Java bootstrap as calling this native on a successful
    /// fetch. Whether it moves the verdict is what `CORDIAL_PRELOAD` is for.
    preload_native: usize,
    /// `nativeInitClientSettingsCachedCompressed`, and the file it is fed.
    ///
    /// The engine writes `flag_cache.dat` itself and Cordial has never handed
    /// one back, so every launch has looked cold to it. Empty path means no
    /// cache was on disk when the plan was built, which is the ordinary first
    /// run.
    cached_native: usize,
    cache_file: String,
    settings: String,
    /// Where `settings` came from, or why it is empty. Printed by
    /// `run_bootstrap`, because the explicit fallback call site already prints
    /// a byte count and the default path did not print anything at all --
    /// see `client_settings::Source` for why that gap mattered.
    settings_source: String,
    flag_names: String,
}

static BOOTSTRAP: std::sync::OnceLock<BootstrapPlan> = std::sync::OnceLock::new();
static BOOTSTRAP_RAN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Deliver settings and flags, from inside the engine's own bootstrap call.
///
/// Prints rather than returning a result because there is nobody to return one
/// to: the caller is the engine, three frames into `initializeNativeCode`.
/// `nativeGameGlobalInit` and `nativeUpdateAdapterInit`, the pair the engine
/// wants before the app bridge.
///
/// Factored out because where they go is under test: §9's captured stack shows
/// the flags failure reporter being reached *through* `nativeGameGlobalInit`, so
/// this pair sits on the path that announces the verdict rather than merely
/// before it. `when` is printed so a log makes it obvious which position a run
/// used, which two earlier orderings did not.
fn call_globals(lib: &linker::Library, when: &str) {
    // `CORDIAL_NO_GLOBAL_INIT=1` is the control for the pair, not a setting.
    //
    // §9's captured stack reaches the failure reporter *through*
    // `nativeGameGlobalInit`, and moving the pair earlier changed nothing. The
    // question that leaves is whether calling it at all is what produces the
    // verdict: on Android the app does not call these directly, the engine's own
    // ActivityNativeMain chain does. If the second `onFlagsFailed` disappears
    // when the pair is skipped, the verdict is localised to this call rather
    // than to the settings handshake, which is worth far more than another
    // variation of the handshake.
    if std::env::var_os("CORDIAL_NO_GLOBAL_INIT").is_some() {
        println!("  globals NOT called ({when}, CORDIAL_NO_GLOBAL_INIT)");
        return;
    }
    for name in [
        "Java_com_roblox_engine_jni_NativeGLInterface_nativeGameGlobalInit",
        "Java_com_roblox_engine_jni_NativeGLInterface_nativeUpdateAdapterInit",
    ] {
        let short = name.rsplit('_').next().unwrap_or(name);
        match lib.symbol(name) {
            None => println!("  {name} not exported"),
            Some(f) => match linker::game_activity::appbridge_call_bare(f) {
                Ok(()) => println!("  {short} ok ({when})"),
                Err(e) => println!("  {short} failed ({when}): {e}"),
            },
        }
    }
}

extern "C" fn run_bootstrap() {
    let Some(plan) = BOOTSTRAP.get() else {
        eprintln!("  bootstrapTheApp: nothing planned");
        return;
    };
    // The engine calls `bootstrapTheApp` twice per launch -- the trace shows two
    // `Call Member Function ... bootstrapTheApp ()V` -- and delivering on both
    // registered two flag providers where Sober registers one. Deliver once.
    if BOOTSTRAP_RAN.swap(true, std::sync::atomic::Ordering::SeqCst) {
        println!("  bootstrapTheApp: already delivered");
        return;
    }
    println!("  bootstrapTheApp: delivering settings and flags");

    // Preloaded overrides first, because "preloaded" is a claim about ordering:
    // the engine reads them inside `getFlags`, which is upstream of the verdict.
    //
    // `CORDIAL_PRELOAD=doc` hands over the same `{"applicationSettings":{...}}`
    // document the settings call gets; `=flat` unwraps it to the bare map, which
    // is the other shape the endpoint's output can be read as; `=empty` sends
    // `{}` to separate "the engine wanted a well-formed document" from "the
    // engine wanted this particular content". Off unless asked for: this is an
    // experiment and shipping an inference as a default is a mistake this file
    // has made once already.
    if plan.preload_native != 0 {
        if let Ok(shape) = std::env::var("CORDIAL_PRELOAD") {
            let body = match shape.as_str() {
                "empty" => "{}".to_string(),
                "flat" => plan
                    .settings
                    .find("\"applicationSettings\"")
                    .and_then(|_| plan.settings.find('{').map(|_| ()))
                    .map(|()| {
                        // Strip the one wrapper key, leaving the bare map. Done
                        // textually rather than with a JSON parse because the
                        // document is 1.2 MB and this is a diagnostic.
                        let open = plan.settings.find(':').map(|i| i + 1).unwrap_or(0);
                        let inner = plan.settings[open..].trim();
                        inner.trim_end_matches('}').trim_end().to_string()
                    })
                    .unwrap_or_else(|| plan.settings.clone()),
                _ => plan.settings.clone(),
            };
            match linker::game_activity::preload_flag_overrides(
                plan.preload_native as *mut std::ffi::c_void,
                &body,
            ) {
                Ok(()) => println!(
                    "    nativePreloadFlagOverrides ok ({shape}, {} bytes)",
                    body.len()
                ),
                Err(e) => println!("    nativePreloadFlagOverrides failed: {e}"),
            }
        }
    }
    // `CORDIAL_CACHED_SETTINGS=1` hands the engine back its own compressed flag
    // cache before the plain document, when one exists.
    //
    // Thirteen candidates have varied the plain three-string path and left the
    // verdict exactly where it was. This is a different path rather than a
    // fourteenth variation of the same one: the engine wrote this file, exports
    // a native that takes it, and has never been given it. Off by default until
    // it is shown to do something.
    if plan.cached_native != 0 && std::env::var_os("CORDIAL_CACHED_SETTINGS").is_some() {
        match std::fs::read(&plan.cache_file) {
            Err(e) => println!("    cached settings: no {} ({e})", plan.cache_file),
            Ok(bytes) => {
                let when = std::fs::metadata(&plan.cache_file)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_millis() as i64);
                // The three strings and the boolean are swept rather than
                // guessed. The first attempt -- all empty, flag true, mtime as
                // the long -- returned 3, which is neither the 0 nor the 1 the
                // plain three-string form produces, so the engine is reading
                // these and rejecting them for a reason worth finding. The
                // result code is the signal; the log says nothing about this
                // call at all.
                //
                //   CORDIAL_CACHED_ARGS=AndroidApp,production,
                //   CORDIAL_CACHED_FLAG=0
                //   CORDIAL_CACHED_WHEN=0
                let args = std::env::var("CORDIAL_CACHED_ARGS").unwrap_or_default();
                let mut it = args.split(',');
                let (a1, a2, a3) = (
                    it.next().unwrap_or(""),
                    it.next().unwrap_or(""),
                    it.next().unwrap_or(""),
                );
                let flag = std::env::var("CORDIAL_CACHED_FLAG")
                    .map_or(true, |v| v != "0");
                let when = std::env::var("CORDIAL_CACHED_WHEN")
                    .ok()
                    .and_then(|v| v.parse::<i64>().ok())
                    .unwrap_or(when);
                match linker::game_activity::init_client_settings_cached_compressed(
                    plan.cached_native as *mut std::ffi::c_void,
                    &bytes,
                    a1,
                    a2,
                    a3,
                    when,
                    flag,
                ) {
                    Ok(code) => println!(
                        "    nativeInitClientSettingsCachedCompressed ({} bytes, [{a1}|{a2}|{a3}], when {when}, flag {flag}) -> {code}",
                        bytes.len()
                    ),
                    Err(e) => println!("    cached settings failed: {e}"),
                }
            }
        }
    }

    // The explicit fallback call site (used when `bootstrapTheApp` is not
    // installed) has always printed "client settings: N bytes" here; this path
    // -- the one every ordinary launch actually takes -- printed nothing at
    // all. GitHub issue #21's reporter A had an empty gap in the log between
    // the early directory setters and the crash, with no way to tell whether
    // the document that produced ten resolved flags out of a hundred and
    // thirty-nine was a bad `--client-settings` path, a fetch that never
    // connected, or a fetch that connected and was refused. This is that line.
    println!(
        "  client settings: {} bytes ({})",
        plan.settings.len(),
        plan.settings_source
    );
    if plan.settings_native != 0 {
        match linker::game_activity::init_client_settings(
            plan.settings_native as *mut std::ffi::c_void,
            &plan.settings,
            "",
            "",
        ) {
            Ok(code) => println!("    nativeInitClientSettings -> {code}"),
            Err(e) => println!("    nativeInitClientSettings failed: {e}"),
        }
    }
    // `post` immediately after `settings`, and the flag names last.
    //
    // That is Sober's order, read off its own log rather than guessed:
    // nativeInitClientSettings at 3.700s, nativePostClientSettingsLoaded
    // Initialization3 at 3.796s, RbxStorage::init at 3.820s, and
    // nativeInitializeNativeFlags only later. The first arrangement here put the
    // flag names between the two and the 139-name list takes long enough that
    // `post` landed after the verdict had already been reported.
    // `CORDIAL_NO_EARLY_POST=1` leaves this to the late call site.
    //
    // **The question this comment used to leave open is answered: the early call
    // is actively harmful, and it is now off by default.** It asked "whether the
    // wasted early call is merely useless or actively harmful, which nobody can
    // answer while both calls are made". Making it optional answered it.
    //
    // It costs `IxpStorageManager`. Same binary, one environment variable apart,
    // arms interleaved in one session: with the early call skipped, the
    // subsystem runs and writes its `ixp_cache_random_id` 10 times out of 10;
    // with it made, 0 out of 50. Both arms still reach `app ready: Landing` and
    // both still build a 45,056-byte `rbx-storage.db`, so the early call costs a
    // subsystem and buys nothing observable.
    //
    // The earlier note that it "was always a no-op" is withdrawn: a call that
    // suppresses a subsystem is not a no-op, it is a side effect nobody had
    // looked for. It produced no `[FLog::AndroidGLView]` line, which is all that
    // was ever established, and absence of a log line was read as absence of an
    // effect -- the same mistake `docs/analysis/flag-init.md` records ten times.
    //
    // `CORDIAL_EARLY_POST=1` restores it for anyone bisecting this. The late
    // call site, which predates the early one and is what actually produces the
    // block, is untouched.
    if plan.post_native != 0 && std::env::var_os("CORDIAL_EARLY_POST").is_some() {
        match linker::game_activity::post_client_settings_loaded(
            plan.post_native as *mut std::ffi::c_void,
        ) {
            Ok(()) => println!("    postClientSettingsLoadedInitialization3 ok"),
            Err(e) => println!("    postClientSettingsLoadedInitialization3 failed: {e}"),
        }
    }
    if plan.flags_native != 0 {
        match linker::game_activity::init_flags(
            plan.flags_native as *mut std::ffi::c_void,
            &plan.flag_names,
        ) {
            Ok(()) => println!("    flags initialised"),
            Err(e) => println!("    flag init failed: {e}"),
        }
    }
}

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

/// Every monitor GDK currently knows about, as `cordial_runtime::refresh::Output`.
///
/// This is `cordial_shell::refresh_watch::outputs` in spirit but not in fact:
/// that function marks an output `current` by asking `gdk::Display::
/// monitor_at_surface` about the specific `gtk::Window` the caller passes it,
/// and there is no such window reachable from here. The engine's own host
/// window is built and kept entirely inside `android::wayland::WaylandWindow`
/// -- a private field (`HostWindowCell`) with no accessor -- and `android/**`
/// was out of scope for the change that wired this up. So every `Output` below
/// carries `current: false`; see `wire_refresh_rate` for what that means for
/// the rate actually reported as current.
///
/// `gdk::Display::default()` answers regardless of that gap, because GTK/GDK
/// is initialised once, process-wide, as a side effect of the engine's window
/// opening (`cordial_shell::host_window::init_wayland`, called from inside
/// `wayland::open`) -- it does not itself need the window object, only that
/// something in the process has already brought GDK up. Empty before that has
/// happened, which `refresh::supported_from`/`current_for` already treat as
/// "nothing plausible is known" rather than a fault.
fn refresh_outputs() -> Vec<cordial_runtime::refresh::Output> {
    let Some(display) = gtk4::gdk::Display::default() else { return Vec::new() };
    let monitors = display.monitors();
    (0..monitors.n_items())
        .filter_map(|i| monitors.item(i))
        .filter_map(|obj| obj.downcast::<gtk4::gdk::Monitor>().ok())
        .map(|m| cordial_runtime::refresh::Output {
            // Reusing `refresh::hz_from_millihertz` rather than repeating its
            // one-line body: `refresh_watch.rs` only re-derives that division
            // because the Cargo cycle noted in its header leaves it no other
            // choice, and load.rs is on the correct side of that edge.
            hz: cordial_runtime::refresh::hz_from_millihertz(m.refresh_rate()),
            current: false,
        })
        .collect()
}

/// Tell the engine what the display can do, and keep it told.
///
/// `NativeGLInterface.nativePassSupportedRefreshRates`/
/// `nativePassCurrentDisplayRefreshRate` are exported by every build this
/// project has looked at and neither had ever been called -- see
/// `cordial_runtime::refresh` for the policy this follows.
///
/// **What this does not achieve.** The design in `refresh.rs` and
/// `refresh_watch.rs` reports "current" as the output the engine's own window
/// is *mostly on*, tracked as the window moves and re-announced through
/// `worth_announcing`. That needs a live `gtk::Window` to call `watch` on, and
/// -- see `refresh_outputs` -- none is reachable from this file. What this
/// does instead: send the real supported-rate list at startup and on every
/// hotplug, and send a "current" rate chosen by `current_for`'s own documented
/// fallback (the first plausible rate, when nothing is marked current) rather
/// than inventing a second heuristic here. On a single-monitor machine that
/// fallback is exact, because there is only one candidate. On a multi-monitor
/// one -- this is true of the machine this was tested on -- it is a real rate
/// of a real attached output, not a fabricated number, but it is **not**
/// verified to be the output the window actually landed on, and must not be
/// read as though it were.
///
/// Window-crosses-a-boundary tracking specifically -- the case
/// `refresh_watch.rs`'s own header calls out -- is therefore not wired by this
/// change. It needs a `pub fn` on `android::wayland::WaylandWindow` (or on
/// `android::WindowHandle`) handing back the `adw::Window`
/// `cordial_shell::host_window::HostWindow::window()` already exposes;
/// `android/**` was off limits to the change that added this function, so
/// that accessor does not exist yet.
fn wire_refresh_rate(lib: linker::Library) {
    let supported_native = lib.symbol(
        "Java_com_roblox_engine_jni_NativeGLInterface_nativePassSupportedRefreshRates",
    );
    let current_native = lib.symbol(
        "Java_com_roblox_engine_jni_NativeGLInterface_nativePassCurrentDisplayRefreshRate",
    );
    println!(
        "  refresh: nativePassSupportedRefreshRates {}",
        if supported_native.is_some() { "resolved" } else { "NOT exported" }
    );
    println!(
        "  refresh: nativePassCurrentDisplayRefreshRate {}",
        if current_native.is_some() { "resolved" } else { "NOT exported" }
    );
    let (Some(supported_native), Some(current_native)) = (supported_native, current_native) else {
        return;
    };

    // Shared between the startup call below and the hotplug callback, so a
    // hotplug that leaves the rate unchanged does not re-announce -- see
    // `refresh::worth_announcing`'s own reasoning for why that matters.
    let previous_current: Rc<Cell<Option<f32>>> = Rc::new(Cell::new(None));
    let announce = {
        let previous_current = previous_current.clone();
        move || {
            let outputs = refresh_outputs();
            let supported = cordial_runtime::refresh::supported_from(&outputs);
            if supported.is_empty() {
                println!("  refresh: no plausible output to report yet");
            } else {
                match linker::game_activity::pass_supported_refresh_rates(supported_native, &supported) {
                    Ok(()) => println!("  refresh: nativePassSupportedRefreshRates {supported:?}"),
                    Err(e) => println!("  refresh: nativePassSupportedRefreshRates failed: {e}"),
                }
            }
            // Only when there is no ambiguity about which output that is.
            //
            // Nothing reachable from here holds the engine's window, so no
            // `Output` built above can carry `current: true`, and
            // `current_for`'s fallback picks the first plausible rate -- which
            // is GDK's enumeration order, not where the window is. On the
            // machine this was written on that is a coin flip between 49.998
            // and 60.002 Hz.
            //
            // Sending it anyway would be telling the engine something specific
            // and unverified, in the one area AGENTS.md is most emphatic about:
            // with input flowing the frame rate is a hard FIFO vsync lock to
            // the output's refresh, so a client that names the wrong output has
            // asked the engine to schedule against a display it is not on. The
            // supported list above is complete and true whatever the window is
            // doing, and goes regardless; this one waits.
            //
            // What unblocks it is small and named: an accessor on
            // `android::wayland::WaylandWindow` handing back the `adw::Window`
            // that `cordial_shell::host_window::HostWindow::window()` already
            // exposes, so `monitor_at_surface` can answer properly.
            let unambiguous = supported.len() == 1;
            let current = if unambiguous {
                cordial_runtime::refresh::current_for(&outputs)
            } else {
                None
            };
            if cordial_runtime::refresh::worth_announcing(previous_current.get(), current) {
                if let Some(hz) = current {
                    match linker::game_activity::pass_current_refresh_rate(current_native, hz) {
                        Ok(()) => println!("  refresh: nativePassCurrentDisplayRefreshRate {hz}"),
                        Err(e) => println!("  refresh: nativePassCurrentDisplayRefreshRate failed: {e}"),
                    }
                }
            } else if !unambiguous && previous_current.get().is_none() {
                println!(
                    "  refresh: {} outputs differ and nothing here knows which the window is on; \
                     not naming a current rate",
                    supported.len()
                );
            }
            previous_current.set(current);
        }
    };

    announce();

    // Hotplug only -- a monitor appearing or disappearing changes
    // `display.monitors()` regardless of where the window is, so this needs
    // no window reference either. GDK's `items-changed` fires from whichever
    // code pumps the process's one `glib::MainContext`, which
    // `android::wayland`'s own pump already does on every tick; nothing here
    // has to add a second pump loop.
    if let Some(display) = gtk4::gdk::Display::default() {
        display.monitors().connect_items_changed(move |_, _, _, _| announce());
    }
}

/// Tell the engine about the battery, and keep it told.
///
/// `NativeGLInterface.reportBatteryStateChanged`/`reportBatteryStatus` are
/// exported by every build this project has looked at and neither had ever
/// been called — see `cordial_runtime::battery` for the sysfs read and for
/// where the two-int call's argument meaning came from (settled that the two
/// things travel together and roughly how often; `INFERRED` on which int is
/// which and their exact numbering).
///
/// Polls `/sys/class/power_supply` every fifteen seconds — the cadence
/// `docs/traces/waydroid-roblox-startup.log.gz` shows the real Android app's
/// own `BatteryStatusObserver` using for this same job, not a number invented
/// here — well below the rate the engine itself polls cpufreq at, and only
/// calls the engine when the reading actually differs from the one last
/// reported, the same "do not re-announce nothing" shape `wire_refresh_rate`
/// above uses `worth_announcing` for.
fn wire_battery_reporting(lib: linker::Library) {
    let state_changed_native =
        lib.symbol("Java_com_roblox_engine_jni_NativeGLInterface_reportBatteryStateChanged");
    let status_native =
        lib.symbol("Java_com_roblox_engine_jni_NativeGLInterface_reportBatteryStatus");
    println!(
        "  battery: reportBatteryStateChanged {}",
        if state_changed_native.is_some() { "resolved" } else { "NOT exported" }
    );
    println!(
        "  battery: reportBatteryStatus {}",
        if status_native.is_some() { "resolved" } else { "NOT exported" }
    );
    let (Some(state_changed_native), Some(status_native)) = (state_changed_native, status_native)
    else {
        return;
    };

    let power_supply_dir = std::path::PathBuf::from("/sys/class/power_supply");
    let last: Rc<Cell<Option<cordial_runtime::battery::Reading>>> = Rc::new(Cell::new(None));

    let report = move || {
        let reading = cordial_runtime::battery::scan(&power_supply_dir);
        let previous = last.take();
        let changed = previous.as_ref() != Some(&reading);
        if changed {
            match cordial_runtime::battery::state_changed_args(&reading) {
                Some((status, plugged)) => {
                    match linker::game_activity::report_battery_state_changed(
                        state_changed_native,
                        status,
                        plugged,
                    ) {
                        Ok(()) => {
                            println!("  battery: reportBatteryStateChanged({status}, {plugged})")
                        }
                        Err(e) => println!("  battery: reportBatteryStateChanged failed: {e}"),
                    }
                }
                // No battery present — see `battery::state_changed_args`'s own
                // doc for why that skips the call rather than inventing a
                // reading for a battery that does not exist.
                None => println!(
                    "  battery: no present battery; reportBatteryStateChanged skipped"
                ),
            }

            let b = reading.battery.as_ref();
            let fields = linker::game_activity::BatteryStatusFields {
                present: b.map(|b| b.present),
                percentage: b.and_then(|b| b.percentage).map(|p| p as i32),
                status: b.map(|b| b.status),
                health: b.and_then(|b| b.health),
                voltage_mv: b.and_then(|b| b.voltage_mv),
                current_now_ua: b.and_then(|b| b.current_now_ua),
                current_avg_ua: b.and_then(|b| b.current_avg_ua),
                charge_counter_uah: b.and_then(|b| b.charge_counter_uah),
                power_now_uw: b.and_then(|b| b.power_now_uw),
                technology: b.and_then(|b| b.technology.clone()),
                // The DTO field is `Float`, not `Integer` — a real degree
                // value, not Android's own tenths-of-a-degree convention. See
                // `native/battery.cpp`'s `CordialBatteryStatus` doc.
                temperature_c: b.and_then(|b| b.temperature_tenths_c).map(|t| t as f32 / 10.0),
                plugged: reading.plugged,
            };
            match linker::game_activity::report_battery_status(status_native, &fields) {
                Ok(()) => println!("  battery: reportBatteryStatus {fields:?}"),
                Err(e) => println!("  battery: reportBatteryStatus failed: {e}"),
            }
        }
        last.set(Some(reading));
    };

    report();

    gtk4::glib::timeout_add_local(std::time::Duration::from_secs(15), move || {
        report();
        gtk4::glib::ControlFlow::Continue
    });
}

/// Install what `cordial_runtime::webview::on_open_window` hands a parsed
/// request to, so an `openWindow` message actually opens something instead of
/// only being logged.
///
/// Called once, right after `webview::arm`, from the same thread that owns
/// GTK's `MainContext` -- the looper thread, which is where every other GTK
/// call this file makes already happens. That matters for *this* call, the
/// one that registers the closure, but not for the closure's own body: the
/// engine can publish `openWindow` from any of its own threads (see
/// `webview::on_open_window`'s doc), so the closure re-enters the GTK thread
/// itself on every call, via `MainContext::default().invoke`, rather than
/// assuming it is already there.
///
/// `#[cfg(feature = "webview")]` because the closure's body ends in
/// `cordial_shell::webview::open`, an actual `WebKitWebView` -- see that
/// feature's own comment in `Cargo.toml`. The `#[cfg(not(...))]` twin below
/// keeps the call site at the top of this function unconditional and says
/// plainly, once, why nothing will render: a build without the feature must
/// not silently swallow every `openWindow` with no explanation, which is
/// exactly the failure this whole module exists to end.
#[cfg(feature = "webview")]
fn install_webview_presenter() {
    cordial_runtime::webview::set_presenter(|request| {
        gtk4::glib::MainContext::default().invoke(move || {
            let Some(window) = cordial_runtime::android::wayland::current() else {
                println!(
                    "[webview] presenter ran with no Wayland host window open; nothing to attach \
                     the web window to"
                );
                return;
            };
            // Fetched here, on the GTK thread, right before it is needed --
            // not inside `on_open_window`, which runs on a thread this crate
            // has never established is safe to block on a Secret Service
            // round trip. See `webview::roblox_session_cookie`'s own doc.
            let cookie =
                cordial_runtime::webview::roblox_session_cookie(&cordial_runtime::profile::active());
            let shell_request = cordial_runtime::webview::to_shell_request(&request, cookie);
            match cordial_shell::webview::open(window.window(), &shell_request) {
                Some(dialog) => {
                    println!("[webview] presented an openWindow request");
                    // The engine's subsurface sits above the host window's
                    // own content by default (see
                    // `android::wayland`'s module doc, "A web-view dialog is
                    // invisible by default, and this is why"), and an
                    // `AdwDialog` draws into that same content -- so without
                    // this the dialog just opened is real and correctly
                    // rendered and the engine is compositing over every pixel
                    // of it. `webview_dialog_opened` lowers the canvas for as
                    // long as this dialog (or any other) is up;
                    // `connect_closed` is libadwaita's own notification that
                    // it no longer is, which is the only reliable place to
                    // raise the canvas back -- there is no `close_request` on
                    // `AdwDialog` this presenter forces, so this is the one
                    // path every dismissal (button, gesture, `closeWindow`)
                    // actually takes.
                    //
                    // Raising the subsurface is necessary and not sufficient
                    // -- reported live, after the stacking fix landed: the
                    // engine blanks its own drawing when it opens a window,
                    // expecting to be covered, and nothing was telling it the
                    // cover was gone, so it stayed blank under a correctly-
                    // stacked, now-visible canvas. `report_window_closed`
                    // is that missing half -- see its own doc for the bus id
                    // this publishes and why that choice is `INFERRED`. Both
                    // calls belong in the same signal for the same reason:
                    // one dismissal path, so the stacking fix and the report
                    // to the engine cannot drift apart and one outlive the
                    // other.
                    use libadwaita::prelude::AdwDialogExt;
                    window.webview_dialog_opened();
                    // **Nothing here touches the cursor any more.** This used
                    // to ask GDK for a `default` on the toplevel, because
                    // Cordial hid the cursor itself from `pointer_enter` and
                    // there was no other way to get one back over a dialog.
                    // The canvas widget now carries `none` and the dialog is
                    // not one of its descendants, so GTK gives it an ordinary
                    // cursor without being asked -- see
                    // `host_window::canvas_cursor`.
                    let host = window;
                    dialog.connect_closed(move |_| {
                        host.webview_dialog_closed();
                        cordial_runtime::webview::report_window_closed();
                    });
                }
                None => println!(
                    "[webview] openWindow request was refused by policy before it could be presented"
                ),
            }
        });
    });
    // The other direction. `set_presenter` above carries the engine's
    // `openWindow` out to a dialog; this carries a command the page issues
    // back in. It is installed here, rather than beside `webview::arm`, for
    // the reason `cordial_shell::webview::set_bridge_sink`'s doc gives:
    // `cordial-runtime` depends on `cordial-shell` and not the reverse, so
    // this binary is the only place that can see both halves at once.
    //
    // Without it the shell's handler has nowhere to send an approved message
    // and says so on every one -- which is the state the maintainer was
    // looking at when Join navigated instead of joining.
    cordial_shell::webview::set_bridge_sink(cordial_runtime::webview::forward_bridge_message);
    println!("  webview: presenter installed; an openWindow request will now be attached to the host window");

    // Said here, at startup, rather than left for the first `openWindow` to
    // discover. The presenter attaches an `AdwDialog` to the GTK host window,
    // and that window only exists on the Wayland backend. On X11 there is no
    // host window to attach to, so every openWindow the engine ever sends is
    // dropped.
    //
    // This is a much narrower case than it was an hour ago, and the history is
    // the reason the warning stays. `android::backend()` used to require an
    // opt-in `CORDIAL_WAYLAND=1`, which `launch.rs` set and a hand-run
    // `cordial-run` did not -- so the invocation AGENTS.md documents defaulted
    // to a backend where the entire web view feature was inert, while the same
    // build launched through `just dev` had it. Running that command and
    // reading "presenter ran with no Wayland host window open" as a bug in the
    // presenter is what cost the time; the presenter was fine and the backend
    // was X11. `backend()` now prefers Wayland whenever `WAYLAND_DISPLAY` is
    // set, so the only ways left to be here are a genuinely display-less host
    // or an explicit `CORDIAL_X11=1` -- both of which someone chose, and
    // neither of which should silently cost them Join.
    //
    // A line at the point of failure is not enough on its own: it names
    // Wayland rather than naming X11, it arrives only once a user has already
    // pressed something, and by then it reads as the button being broken.
    // AGENTS.md's rule against a stub that lies applies to a diagnostic too --
    // reporting the gap up front is what keeps it findable.
    if cordial_runtime::android::wayland::current().is_none() {
        println!(
            "  webview: WARNING -- there is no Wayland host window, so nothing can be attached \
             to and every openWindow (Join, sign-in, Robux) will be dropped. This run is on the \
             X11 backend -- either CORDIAL_X11 is set, or there is no WAYLAND_DISPLAY to use. \
             Web views need the Wayland backend."
        );
    }
}

/// Without an embedded web view, send the engine's `openWindow` to the user's
/// browser instead of dropping it.
///
/// **This used to install nothing at all.** It printed a line saying an
/// `openWindow` would be "parsed and logged but nothing will be shown", and
/// that is exactly what happened: the engine asked for a window, the request
/// was parsed, and it went in the bin. From the outside that is a link that
/// does nothing, which is how it was reported -- "clicking a link on sober
/// takes you to a website via xdg open, on cordial it doesnt".
///
/// Handing it to the browser is strictly better than dropping it and is what
/// the user is asking for by clicking. It is deliberately *only* the
/// no-web-view build: where a real web view exists the engine gets the
/// in-application window it asked for, because some of these are sign-in and
/// checkout flows that expect to come back.
///
/// Same gate as `Linking.openURL` and for the same reason -- the URL comes
/// from the engine, which got it from Lua. `urlopen` refuses anything that is
/// not http or https, and the address is never logged because it can carry
/// credentials in its query string.
#[cfg(not(feature = "webview"))]
fn install_webview_presenter() {
    println!(
        "  webview: built without the `webview` feature (needs webkitgtk6.0-devel); \
         openWindow will open in your browser instead of an in-app window -- see \
         `just build toolbox` for the embedded one"
    );
    cordial_runtime::webview::set_presenter(|request| {
        match cordial_plugins::urlopen::open(&request.url) {
            Ok(()) => println!("  webview: openWindow handed to the browser"),
            Err(e) => println!("  webview: openWindow could not be opened: {e}"),
        }
    });
}

/// The engine's own version, read out of `libroblox.so` rather than hardcoded.
///
/// This existed as a hardcoded `"2.732.0.1043"` with a comment claiming it was
/// "the engine's own answer rather than a guess". It was neither: the engine in
/// the APK on this machine is **2.730.0.790**, which is what it stamps on every
/// log file it writes, so Cordial was telling the server one version while the
/// client was another. A build that misreports its own version is exactly the
/// shape of thing a server-side check rejects, and the value had gone stale
/// silently across an APK update with nothing to catch it.
///
/// The scan itself moved to [`cordial_update::engine`]. It was thirty lines
/// here, in a binary, which meant the updater could not call it and had to
/// report that it did not know which build was installed — while this function
/// printed the answer at every launch. One copy, in the crate that compares it
/// against what Roblox has published; the rules are unchanged, including
/// returning `None` when the four-part shape is not unique, because skipping
/// the call is honest and inventing a version is what caused the bug above.
fn engine_version(lib_dir: &str) -> Option<String> {
    cordial_update::engine::installed_version(std::path::Path::new(lib_dir))
}

// `native/local_storage.cpp`'s two exported callers. Declared directly here
// rather than through `cordial_linker_sys::game_activity` -- that module is
// the usual home for a wrapper like this, and it was off limits to the task
// that added these two, on the reasoning that a crate several agents rely on
// as a stable interface should not gain new surface mid-session. The symbols
// still link in exactly the same way: `native/CMakeLists.txt` compiles
// `local_storage.cpp` into the same `libcordial_jni_shim.a`
// `cordial-linker-sys`'s `build.rs` already tells `cordial-run` to link, so a
// bare `extern "C"` here resolves at the same final link step every other
// wrapper in that crate does.
extern "C" {
    fn cordial_local_storage_set_platform_impl(
        f: *mut std::ffi::c_void,
        err: *mut std::os::raw::c_char,
        err_len: usize,
    ) -> std::os::raw::c_int;
    fn cordial_update_screen_orientation(
        f: *mut std::ffi::c_void,
        width: std::os::raw::c_int,
        height: std::os::raw::c_int,
        err: *mut std::os::raw::c_char,
        err_len: usize,
    ) -> std::os::raw::c_int;
}

fn take_c_err(err: Vec<u8>) -> String {
    let end = err.iter().position(|&b| b == 0).unwrap_or(err.len());
    String::from_utf8_lossy(&err[..end]).into_owned()
}

/// `ILocalStorageHandlerCore.setPlatformImpl(IPlatformLocalStorageHandler)`.
/// See `native/local_storage.cpp` for what the object handed over answers and
/// why the call is believed to be static.
fn local_storage_set_platform_impl(f: *mut std::ffi::c_void) -> Result<(), String> {
    let mut err = vec![0u8; 512];
    // SAFETY: `f` is the exported JNI native the caller resolved by name;
    // `err` is a live buffer for the duration of this call.
    let rc = unsafe {
        cordial_local_storage_set_platform_impl(f, err.as_mut_ptr() as *mut std::os::raw::c_char, err.len())
    };
    if rc == 0 { Ok(()) } else { Err(take_c_err(err)) }
}

/// `NativeInputInterface.nativeUpdateScreenOrientation(I)V` -- the one call
/// `docs/analysis/flag-init.md` §16 found mocktail makes between
/// `initializeNativeCode` and the settings handshake that Cordial did not.
fn update_screen_orientation(f: *mut std::ffi::c_void, width: i32, height: i32) -> Result<(), String> {
    let mut err = vec![0u8; 512];
    // SAFETY: as above.
    let rc = unsafe {
        cordial_update_screen_orientation(
            f,
            width,
            height,
            err.as_mut_ptr() as *mut std::os::raw::c_char,
            err.len(),
        )
    };
    if rc == 0 { Ok(()) } else { Err(take_c_err(err)) }
}

/// Take this instance's claim on its profile, or produce the refusal that says
/// why the client is not starting.
///
/// **ADR-012 says a profile is held by at most one instance at a time, and
/// until 2026-08-22 that was only true of clients the shell launched.**
/// `launch.rs` builds a claim and `Claim::hand_to` passes it down; `cordial-run`
/// had no claim code at all, so `cordial-run --profile X` — the invocation
/// AGENTS.md documents — took no lock whatsoever. Four of them ran against one
/// profile that day and not one was refused, which is precisely the two-writers
/// corruption the ADR exists to prevent, in the command every contributor here
/// is told to type.
///
/// `claim_for_instance` rather than `acquire` because both entry points have to
/// work. A `flock` belongs to the open file description, so a client that
/// always opened the lock file and locked it would be refused by the lock it
/// had just inherited from the shell — every shell launch would fail. See that
/// function for the adoption path and what it verifies before believing it.
///
/// The name, not `profile::active()`: `active()` is a path, and the shell's
/// `acquire` wants the name it validates. They cannot disagree about where that
/// lands — `cordial_runtime::profile::root()` is now the shell's own `root()`
/// rather than a second copy of the same environment walk.
fn claim_profile(opt: &Options) -> Result<cordial_shell::profile::Claim, String> {
    let name = opt.profile.as_deref().unwrap_or(cordial_runtime::profile::DEFAULT_NAME);
    match cordial_shell::profile::claim_for_instance(name) {
        Ok(claim) => Ok(claim),
        Err(e) => Err(match e.advice() {
            // A refusal, not a crash, and it has to read as one. Everybody who
            // has been running two clients against `default` starts being
            // stopped here, so the message carries its own explanation and the
            // separate-data-root recipe from AGENTS.md rather than a bare line
            // about a lock.
            Some(advice) => format!("{e}\n\n{advice}"),
            None => e.to_string(),
        }),
    }
}

/// Free bytes on the filesystem holding `path`, or `None` if it cannot be asked.
///
/// `df` rather than a `statvfs` binding, because this is a diagnostic and
/// adding a crate for it would be the wrong trade. `None` is "unknown" and must
/// never be reported as "full".
fn free_bytes(path: &std::path::Path) -> Option<u64> {
    let mut probe = path.to_path_buf();
    while !probe.exists() {
        probe = probe.parent()?.to_path_buf();
    }
    let out = std::process::Command::new("df")
        .args(["-Pk", "--output=avail"])
        .arg(&probe)
        .output()
        .ok()?;
    String::from_utf8(out.stdout)
        .ok()?
        .lines()
        .nth(1)?
        .trim()
        .parse::<u64>()
        .ok()
        .map(|kb| kb * 1024)
}

/// Say so, loudly, when the disk is nearly full.
///
/// **A game that dies because it could not write is the least self-explanatory
/// failure Cordial produces.** Roblox writes its own storage, its logs and its
/// asset cache under the profile; when those writes fail the engine does not
/// report a disk error, it crashes or wedges somewhere with no relationship to
/// the cause, and Cordial said nothing about it either -- not on screen and not
/// in the log. Reported by a maintainer who hit exactly that.
///
/// Two thresholds, because the two moments want different words. Before a
/// launch there is still time to do something about it; afterwards the only
/// useful thing to say is that this is probably why.
fn report_disk(where_: &std::path::Path, when: DiskMoment) {
    let Some(free) = free_bytes(where_) else { return };
    if let Some(line) = disk_warning(free, when, where_) {
        println!("{line}");
    }
}

/// What to say about `free` bytes, or nothing.
///
/// Split out from [`report_disk`] because the interesting part is the decision
/// and the decision is untestable through the filesystem: a test cannot fill a
/// disk, and one that could would be filling the developer's. Same arrangement,
/// and the same reason, as `updater::dressing`.
fn disk_warning(free: u64, when: DiskMoment, where_: &std::path::Path) -> Option<String> {
    const LOW: u64 = 2 * 1024 * 1024 * 1024;
    const CRITICAL: u64 = 512 * 1024 * 1024;
    let mb = free / 1_048_576;
    let at = where_.display();
    match when {
        DiskMoment::BeforeLaunch if free < CRITICAL => Some(format!(
            "\n  *** {mb} MB free on {at}. Roblox writes its storage, logs and asset cache \
             here, and at this little space it will probably fail in a way that does not \
             mention the disk. ***\n"
        )),
        DiskMoment::BeforeLaunch if free < LOW => Some(format!(
            "  warning: {mb} MB free on {at}; Roblox writes its storage and asset cache here"
        )),
        DiskMoment::AfterExit if free < CRITICAL => Some(format!(
            "\n  *** the client stopped with only {mb} MB free on {at}. A failed write is the \
             most likely cause: the engine does not report a full disk, it fails somewhere \
             unrelated to it. ***"
        )),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DiskMoment {
    BeforeLaunch,
    AfterExit,
}

fn main() -> ExitCode {
    // **Before `parse()`, and that is not stylistic.** `--profile` latches the
    // active profile directory as a side effect of being parsed, and the whole
    // point of `--headless` is that the compositor must already be this
    // process's parent before anything touches a display or a profile. Handing
    // over here means the child does all of it once, properly, inside the
    // nested compositor.
    if !cordial_runtime::headless::is_child() {
        if cordial_runtime::headless::nested_argv(&std::env::args_os().collect::<Vec<_>>())
            .is_some()
        {
            match cordial_runtime::headless::exec_nested() {
                Ok(never) => match never {},
                Err(msg) => {
                    eprintln!("error: {msg}");
                    return ExitCode::from(2);
                }
            }
        }
    }

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

    // Before anything reads or writes the profile at all, and before the
    // network gate below, which reads a setting out of it.
    //
    // Held in a binding for the rest of `main`: the lock belongs to the open
    // file description, so this value *is* the claim, and dropping it early
    // would release the profile with the engine still running against it. The
    // process exiting — cleanly, by panic, or by SIGKILL — closes the
    // descriptor and releases it, which is the property a lock file holding a
    // PID would not have.
    let _claim = match claim_profile(&opt) {
        Ok(claim) => claim,
        Err(refusal) => {
            eprintln!("error: {refusal}");
            return ExitCode::from(3);
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
        // Before the engine reads a single asset, and therefore long before
        // `plugin_host::start_all` further down: an overlay registered after
        // the engine has already loaded a texture cannot change it, because
        // the bytes are cached and the engine holds a pointer into them
        // (ADR-010's caching note). A data-only plugin has no process to
        // register anything of its own, so this is the only point at which a
        // texture pack can take effect at all.
        let n = cordial_runtime::plugin_host::register_static_overlays();
        if n > 0 {
            println!("  {n} plugin asset overlay(s) registered");
        }
        if cordial_runtime::android::asset::start_watcher() {
            println!("  overlay: watching for changes (CORDIAL_OVERLAY_WATCH)");
        }
    }
    enter_run_dir(&mut opt);

    // Answers "which of my mod's files can never apply" without starting the
    // engine, which is the point: the check is against the APK's own entry
    // list, so it needs an archive and an overlay stack and nothing else. The
    // weaker signal -- a file that exists in the build but was never asked for
    // -- deliberately is not offered here, because it can only be honest after
    // a session that actually played something (ADR-021).
    if opt.check_overlays {
        // Already registered above, with the APK, so this only reads.
        let index = cordial_runtime::android::asset::index();
        println!("overlay: {} file(s) across every registered layer", index.len());
        // Named rather than guessed: if the scan cannot find a version, the
        // report says which archive it checked against instead of inventing
        // one. "no longer matches anything in client <something wrong>" is a
        // worse answer than naming the file.
        let label = cordial_runtime::android::asset::client_version()
            .unwrap_or_else(|| opt.apk.clone().unwrap_or_else(|| "this build".into()));
        match cordial_runtime::android::asset::apk_asset_names() {
            Ok(apk) => {
                let lines = cordial_runtime::android::asset::stale_report(&apk, &label);
                if lines.is_empty() {
                    println!("overlay: every overlay file matches something in this build");
                }
                for line in lines {
                    println!("overlay: {line}");
                }
                for orphan in cordial_runtime::android::asset::stale(&apk) {
                    println!("    stale  {} ({})", orphan.name, orphan.source.describe());
                }
            }
            Err(e) => {
                eprintln!("overlay: cannot read the APK's asset list ({e})");
                return ExitCode::FAILURE;
            }
        }
        for line in cordial_runtime::android::asset::shadow_report() {
            println!("overlay: {line}");
        }
        return ExitCode::SUCCESS;
    }

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

    // Before the table is built, because `symtab::build` consults the same
    // selection to decide whether `libaaudio.so` exists at all, and a reader
    // of this log should see the choice before its consequence.
    cordial_runtime::bionic::announce_audio_backend();

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

    // EXPERIMENTAL, cordial-agent-defer: see docs/analysis/flag-init.md §26.
    // `RbxStorage::init` runs during libroblox.so's own ELF constructors —
    // before Cordial has told the engine any directory — fails on an empty
    // path, and memoises that failure permanently (a lazy singleton). This
    // asks the linker to hold the constructors back so a minimal directory
    // setup can run first; `defer_next_ctors` is consumed by the very next
    // `dlopen`, and `run_deferred_ctors` below runs whatever it postponed.
    // Off by default. Not part of the ordinary load path.
    let defer_ctors = std::env::var_os("CORDIAL_DEFER_CTORS").is_some();
    // EXPERIMENTAL, cordial-android-libpath: see docs/analysis/flag-init.md
    // §31. Sober's engine mapped `libroblox.so` from an authentic Android
    // install path with zero altered bytes; Cordial maps the identical engine
    // from a cache directory shaped like nothing Android ever produces. This
    // overrides what `dladdr()` reports for the library's own load address —
    // the one form of self-location available before `JNI_OnLoad`, since the
    // failing `stat("")` calls run during ELF construction, strictly earlier
    // — to test whether the engine derives its private data directory from
    // it. Also needs constructors deferred, for the same reason as above: the
    // override has to be in place before whatever reads it runs.
    let android_libpath = std::env::var_os("CORDIAL_ANDROID_LIBPATH").is_some();
    if defer_ctors || android_libpath {
        linker::defer_next_ctors(true);
    }

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

    if android_libpath {
        // Sober's own mapping, for shape reference (flag-init.md §31):
        //   /data/app/~~<hash>/com.roblox.client-<hash>/lib/x86_64/libroblox.so
        // The exact hashes are meaningless to the derivation this is testing
        // — only the directory shape (a `~~`-prefixed app id, a
        // package-name-plus-hash directory, then `lib/x86_64/`) can matter,
        // since that is all a string-walk from the tail could look for.
        let synthetic = std::env::var("CORDIAL_ANDROID_LIBPATH_VALUE").unwrap_or_else(|_| {
            "/data/app/~~cordialAAAAAAAAAAAAAA==/com.roblox.client-cordialBBBBBBBBBBBB==\
             /lib/x86_64/libroblox.so"
                .to_string()
        });
        linker::set_realpath(lib, &synthetic);
        println!("\ncordial-android-libpath: soinfo realpath overridden to {synthetic}");
        if !defer_ctors {
            // Nobody else will run the deferred constructors in this case;
            // do it right after the override, so the override is the only
            // variable this changes relative to the ordinary load path.
            println!("cordial-android-libpath: running the deferred constructors now");
            linker::run_deferred_ctors(lib);
            println!("cordial-android-libpath: constructors returned without crashing");
        }
    }

    // EXPERIMENTAL, cordial-agent-defer, continued. Constructors have not
    // run yet if `defer_ctors`: `libroblox.so`'s own global state, including
    // whatever `RbxStorage::init` reads, does not exist. This calls exactly
    // the four `NativeSettingsInterface` directory setters `--game-activity`
    // otherwise calls only after a window and JNI_OnLoad, then runs the
    // constructors that were held back. It does not touch JNI_OnLoad or any
    // other bring-up step; those still happen later, unchanged, once this
    // block returns.
    if defer_ctors {
        println!("\ncordial-agent-defer: constructors deferred for {}", opt.library);
        let Some(vm) = linker::jni::create_vm() else {
            eprintln!("cordial-agent-defer: could not create a JavaVM to call the setters with");
            return ExitCode::FAILURE;
        };
        println!("cordial-agent-defer: JavaVM at {vm:p} (Cordial's own jnivm; libroblox.so's constructors have not run)");

        let root = std::env::var("CORDIAL_FILES_DIR")
            .unwrap_or_else(|_| format!("{}/data", cordial_runtime::profile::active().display()));
        let files = format!("{root}/files");
        let cache = format!("{root}/cache");
        let external = format!("{root}/external");
        for d in [&files, &cache, &external] {
            if let Err(e) = std::fs::create_dir_all(d) {
                println!("  could not create {d}: {e}");
            }
        }
        // Same tree `--game-activity` creates before RbxStorage is expected
        // to look for it — see the comment at its other call site for why
        // each of these exists and where the list came from.
        for base in [root.as_str(), "."] {
            for rel in [
                "files", "cache", "shared_prefs", "rbx-storage", "appData",
                "appData/LocalStorage", "appData/rbx-storage", "appData/ClientSettings",
                "files/appData", "files/appData/LocalStorage", "files/appData/OTAPatchBackups",
                "files/appData/rbx-storage", "cache/ContentProvider_2", "cache/rbx-storage",
                "cache/sounds",
                // The external-storage tree. mocktail creates these three and
                // Cordial did not, which made this an incomplete adoption of a
                // list that was otherwise copied wholesale -- see the other
                // call site for what the rest are for.
                "sdcard/Android/data/com.roblox.client",
                "sdcard/Android/data/com.roblox.client/files",
                "sdcard/Android/data/com.roblox.client/cache",
            ] {
                let _ = std::fs::create_dir_all(format!("{base}/{rel}"));
            }
        }

        const SETTINGS: &str = "com/roblox/engine/jni/NativeSettingsInterface";
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
        for (name, args) in dirs {
            match lib.symbol(name) {
                None => println!("  {name} not exported (pre-ctors)"),
                Some(f) => match linker::game_activity::call_static_strings(f, SETTINGS, args) {
                    Ok(()) => println!(
                        "  {} ok (pre-ctors)",
                        name.rsplit('_').next().unwrap_or(name)
                    ),
                    Err(e) => println!("  {name} failed (pre-ctors): {e}"),
                },
            }
        }

        // EXPERIMENTAL, cordial-agent-defer, second step: `CORDIAL_DEFER_PAST_SETTINGS=1`
        // additionally delivers client settings and flags before running the
        // deferred constructors, bypassing `bootstrapTheApp`'s normal
        // callback route entirely (that route needs `initializeNativeCode`,
        // which needs constructors already run — so it cannot be reached
        // this early). Tests the actual ordering hypothesis directly: on
        // Android, `flagLoaded` succeeds at 0.4158s, strictly after
        // `nativeInitClientSettings` at 0.3752s (docs/analysis/flag-init.md
        // §26.1) — this asks whether matching that order, rather than
        // merely setting directories, is what storage needs.
        if std::env::var_os("CORDIAL_DEFER_PAST_SETTINGS").is_some() {
            const FLAG_NAMES: &str = include_str!("../native-flag-names.txt");
            let settings_json =
                cordial_runtime::client_settings::load(opt.client_settings.as_deref())
                    .unwrap_or_default();
            println!(
                "cordial-agent-defer: fetched {} bytes of client settings (pre-ctors)",
                settings_json.len()
            );
            if let Some(f) = lib.symbol(
                "Java_com_roblox_engine_jni_NativeGLInterface_nativeInitClientSettings",
            ) {
                match linker::game_activity::init_client_settings(f, &settings_json, "", "") {
                    Ok(code) => println!("  nativeInitClientSettings -> {code} (pre-ctors)"),
                    Err(e) => println!("  nativeInitClientSettings failed (pre-ctors): {e}"),
                }
            } else {
                println!("  nativeInitClientSettings not exported (pre-ctors)");
            }
            if let Some(f) = lib.symbol(
                "Java_com_roblox_client_flags_FlagJniInterface_nativeInitializeNativeFlags",
            ) {
                match linker::game_activity::init_flags(f, FLAG_NAMES) {
                    Ok(()) => println!("  flags initialised (pre-ctors)"),
                    Err(e) => println!("  flag init failed (pre-ctors): {e}"),
                }
            } else {
                println!("  nativeInitializeNativeFlags not exported (pre-ctors)");
            }
        }

        println!("cordial-agent-defer: running the deferred constructors now");
        linker::run_deferred_ctors(lib);
        println!("cordial-agent-defer: constructors returned without crashing");
    }

    match lib.symbol("JNI_OnLoad") {
        Some(p) => println!("  JNI_OnLoad {p:p}"),
        None => println!("  JNI_OnLoad not found"),
    }

    if opt.jni_onload {
        if let Some(p) = lib.symbol("JNI_OnLoad") {
            // EXPERIMENTAL, cordial-agent-defer: the defer block above already
            // created the process JavaVM (it needed one to call the directory
            // setters), so `create_vm()` here correctly reports "one already
            // exists" rather than failing. `linker::jni::call_on_load` reaches
            // for the global VM itself and does not need this pointer, so
            // reusing it changes nothing about what JNI_OnLoad is called with.
            let vm = match linker::jni::create_vm() {
                Some(vm) => vm,
                None if defer_ctors => std::ptr::null_mut(),
                None => {
                    eprintln!("could not create a JavaVM");
                    return ExitCode::FAILURE;
                }
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
                        "\n  Roblox expects the Android bring-up sequence, not a bare \
                         JNI_OnLoad:\n  a JavaVM, then \
                         GameActivity.initializeNativeCode called from Java with a real \
                         Activity. See docs/framework-api-inventory.md §3.3."
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
                        // Kept separately because `files` is moved into a
                        // closure further down, and the disk report needs the
                        // path at two moments either side of that.
                        let data_root = std::path::PathBuf::from(&files);
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
                        // Skipped when `bootstrapTheApp` is going to do the
                        // delivery, which is the default. Both running meant the
                        // engine registered a flag provider per call: Cordial
                        // logged `Registered Flag Provider ID from Java:` 0, 1
                        // and 2 on one launch where Sober logs 0 and nothing
                        // else. Whether repeated registration is harmful is not
                        // established, but matching the real client costs
                        // nothing and an unnecessary difference on the path
                        // being investigated is worth removing.
                        if let Some(f) = lib
                            .symbol(
                                "Java_com_roblox_engine_jni_NativeGLInterface_nativeInitClientSettings",
                            )
                            // `CORDIAL_EARLY_SETTINGS=1` decouples this from
                            // the bootstrap switch, which is a combination
                            // nobody has run.
                            //
                            // The early call was added because the first
                            // `flags FAILED` was seen arriving before
                            // `nativeInitClientSettings` had been called at
                            // all — the settings were being delivered after
                            // the decision they were meant to inform. But it
                            // was wired behind `CORDIAL_NO_BOOTSTRAP`, so
                            // "settings before `initializeNativeCode`" and
                            // "no bootstrap" have only ever been true together,
                            // and the useful half has never been tested alone.
                            //
                            // §12 measured the verdict being reached inside
                            // `initializeNativeCode`, before any settings call.
                            // If the engine wants its flags already present when
                            // that runs, this is the shape of the fix and the
                            // coupling is why it looked like it had been ruled
                            // out. Off by default: it is an experiment, and
                            // shipping an inference as a default is a mistake
                            // this file has made once already.
                            .filter(|_| {
                                (std::env::var_os("CORDIAL_NO_BOOTSTRAP").is_some()
                                    || std::env::var_os("CORDIAL_EARLY_SETTINGS").is_some())
                                    && std::env::var_os("CORDIAL_LATE_SETTINGS").is_none()
                            })
                        {
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

                            // `CORDIAL_SETTLE_MS=5000` holds here before
                            // `initializeNativeCode` runs.
                            //
                            // This exists to kill a hypothesis rather than to
                            // ship: the flags verdict arrives 4-6 ms after the
                            // engine calls `bootstrapTheApp`, and every reading
                            // of that as "the verdict outran the settings"
                            // survived because the two were always milliseconds
                            // apart. Putting seconds between them settles it in
                            // one run instead of another round of inference.
                            if let Some(ms) = std::env::var("CORDIAL_SETTLE_MS")
                                .ok()
                                .and_then(|v| v.parse::<u64>().ok())
                            {
                                println!("  settling for {ms} ms before initializeNativeCode");
                                std::thread::sleep(std::time::Duration::from_millis(ms));
                            }
                        }

                        // EXPERIMENTAL, `CORDIAL_EARLY_DIRS=1`: the four
                        // `NativeSettingsInterface` directory setters here,
                        // rather than only after `initializeNativeCode` has
                        // returned.
                        //
                        // The engine composes the flag cache's tombstone path
                        // once, during `postClientSettingsLoadedInitialization3`
                        // — which the engine calls from `bootstrapTheApp`,
                        // inside the `initializeNativeCode` below — and then
                        // keeps it. A `CORDIAL_TRACE_PATHS=1` run shows
                        // `fopen("cache/tombstone.dat") = null` 667 ms before
                        // `nativeSetCacheDirectory ok`, and that same relative
                        // path written again 5.2 s after it, while
                        // `flag_cache.dat` in the same run is absolute because
                        // the writer rebuilds that path per call. So the
                        // tombstone records what the engine's cache directory
                        // was at bootstrap time, and under Cordial that is the
                        // empty string.
                        //
                        // **This is what finally brings `RbxStorage` up.**
                        // `files` and `cache` both set here, and a real
                        // `rbx-storage.db` with rows appears: five runs out of
                        // five, against eight in the same session that produced
                        // none — four with this off, two with only `cache`
                        // early and two with only `files` early
                        // (docs/analysis/flag-init.md §46). Neither directory
                        // alone does it. The engine builds its content store
                        // out of a permanent cache under the files directory
                        // and a temporary one under the cache directory, so
                        // being handed one of the two is being handed neither.
                        //
                        // Defaults to `files,cache` for that reason.
                        // `CORDIAL_EARLY_DIRS=off` is the control; `all` (or
                        // `1`) moves all four; a comma-separated subset of
                        // `files`, `cache`, `external`, `base` picks them
                        // individually, which is how the attribution above was
                        // made.
                        //
                        // The earlier note here claimed 25 runs with only
                        // `cache` early produced a store and that `cache` was
                        // "the only one of the four that storage needs". Both
                        // halves are withdrawn: no data root on this machine
                        // holds a store from such a run, and `cache` alone was
                        // re-run twice here and produced none. What `cache`
                        // alone does produce is the absolute tombstone path,
                        // which is what §44.8 partitioned on -- so the
                        // tombstone form is a co-symptom of an early cache
                        // directory and not a marker for the store, and a run
                        // with `tomb=ABS` and no database is now on record.
                        //
                        // Why the tombstone moves at all, kept because it is
                        // measured and still true: the engine composes that
                        // path once, during
                        // `postClientSettingsLoadedInitialization3` inside the
                        // `initializeNativeCode` below, and then keeps it. A
                        // `CORDIAL_TRACE_PATHS=1` run shows
                        // `fopen("cache/tombstone.dat") = null` 667 ms before
                        // `nativeSetCacheDirectory ok`, and the same relative
                        // path written again 5.2 s after it, while
                        // `flag_cache.dat` in that run is absolute because its
                        // writer rebuilds the path per call.
                        //
                        // **`files` early needs the trust store**, which is why
                        // `enter_run_dir` links `cacert.pem` under the files
                        // directory as well as the run directory. Measured, in
                        // this session, by gating that one link off and
                        // changing nothing else: `error adding trust anchors
                        // from file`, a hundred HTTP error lines, and a run
                        // that never reaches the landing page. So the early
                        // `files` call is skipped when that link is not in
                        // place rather than silently taking HTTPS down with it
                        // -- the bundle is extracted from the APK by
                        // `asset_folder`, which does not run until the app
                        // bridge, so a first launch into an empty asset cache
                        // has no `cacert.pem` to link at all.
                        let which = std::env::var("CORDIAL_EARLY_DIRS")
                            .unwrap_or_else(|_| "files,cache".to_string());
                        if which != "off" && which != "0" {
                            let want = |k: &str| {
                                which == "1" || which == "all"
                                    || which.split(',').any(|p| p.trim() == k)
                            };
                            const SETTINGS: &str =
                                "com/roblox/engine/jni/NativeSettingsInterface";
                            let external = format!("{root}/external");
                            if let Err(e) = std::fs::create_dir_all(&external) {
                                println!("  could not create {external}: {e}");
                            }
                            let early: &[(&str, Vec<&str>)] = &[
                                ("nativeSetFilesDirectory", vec![files.as_str()]),
                                ("nativeSetCacheDirectory", vec![cache.as_str()]),
                                ("nativeSetExternalDirectory", vec![external.as_str()]),
                                (
                                    "nativeSetBaseDataDirectories",
                                    vec![files.as_str(), cache.as_str()],
                                ),
                            ];
                            for (name, args) in early {
                                let key = name
                                    .strip_prefix("nativeSet")
                                    .unwrap_or(name)
                                    .split("Director")
                                    .next()
                                    .unwrap_or(name)
                                    .to_ascii_lowercase();
                                let key = key.strip_suffix("data").unwrap_or(&key);
                                if !want(key) {
                                    continue;
                                }
                                // See the note above: the files directory early
                                // moves where curl looks for its CA bundle, so
                                // without the link there this call is the
                                // difference between a working client and one
                                // whose every HTTPS request fails.
                                if key == "files"
                                    && !std::path::Path::new(&format!("{files}/exe/cacert.pem"))
                                        .exists()
                                {
                                    println!(
                                        "  nativeSetFilesDirectory NOT set early: no {files}/exe/cacert.pem, and early would break HTTPS"
                                    );
                                    continue;
                                }
                                let sym = format!(
                                    "Java_com_roblox_engine_jni_NativeSettingsInterface_{name}"
                                );
                                match lib.symbol(&sym) {
                                    None => println!("  {name} not exported (early)"),
                                    Some(f) => match linker::game_activity::call_static_strings(
                                        f, SETTINGS, args,
                                    ) {
                                        Ok(()) => println!("  {name} ok (early)"),
                                        Err(e) => println!("  {name} failed (early): {e}"),
                                    },
                                }
                            }
                        }

                        // Install the bootstrap before the engine can call it.
                        // `initializeNativeCode` calls `bootstrapTheApp` and
                        // reads the flags verdict immediately after, so this is
                        // the last line at which it can be installed at all.
                        //
                        // `CORDIAL_NO_BOOTSTRAP=1` is the control: it leaves the
                        // hook installed but with nothing behind it, which
                        // reproduces the old behaviour in the same session.
                        // `CORDIAL_LATE_SETTINGS=1` moves the whole handshake
                        // to after the app bridge, which is **not** where Sober
                        // does it -- that half of this note was wrong and is
                        // corrected here rather than left to mislead the next
                        // person into thinking the switch reproduces Sober.
                        // Sober's own log, read today, has
                        // `nativeInitClientSettings` at 7.5499,
                        // `postClientSettingsLoadedInitialization3` at 7.5676,
                        // `RbxStorage::init [INIT]` at 7.5770 and
                        // `nativeAppBridgeV2Init` at 7.5873: the handshake runs
                        // *before* the bridge, by 20 ms, and the store comes up
                        // between them. mocktail and the Waydroid capture have
                        // the same order. The rest of the note stands -- Sober's
                        // bridge does follow `RbxStorage::init`, and Cordial's
                        // is the first line in its own log file. See
                        // flag-init.md §45.4 for the three captures side by
                        // side, and §46 for why that ordering turned out not to
                        // be the thing that was missing.
                        let late = std::env::var_os("CORDIAL_LATE_SETTINGS").is_some();
                        if std::env::var_os("CORDIAL_NO_BOOTSTRAP").is_none() && !late {
                            const FLAG_NAMES: &str = include_str!("../native-flag-names.txt");
                            // Read once, ahead of the struct literal, because
                            // both `settings` and `settings_source` below come
                            // from this one call and a struct literal cannot
                            // share a `let` between two of its own fields.
                            let (settings_body, settings_source) =
                                cordial_runtime::client_settings::load_reporting(
                                    opt.client_settings.as_deref(),
                                );
                            let plan = BootstrapPlan {
                                settings_native: lib
                                    .symbol("Java_com_roblox_engine_jni_NativeGLInterface_nativeInitClientSettings")
                                    .map_or(0, |p| p as usize),
                                post_native: lib
                                    .symbol("Java_com_roblox_engine_jni_NativeGLInterface_nativePostClientSettingsLoadedInitialization3")
                                    .map_or(0, |p| p as usize),
                                flags_native: lib
                                    .symbol("Java_com_roblox_client_flags_FlagJniInterface_nativeInitializeNativeFlags")
                                    .map_or(0, |p| p as usize),
                                preload_native: lib
                                    .symbol("Java_com_roblox_client_startup_MainGameActivity_nativePreloadFlagOverrides")
                                    .map_or(0, |p| p as usize),
                                cached_native: lib
                                    .symbol("Java_com_roblox_engine_jni_NativeGLInterface_nativeInitClientSettingsCachedCompressed")
                                    .map_or(0, |p| p as usize),
                                cache_file: format!("{cache}/cache/flag_cache.dat"),
                                settings: settings_body.unwrap_or_default(),
                                settings_source: settings_source.to_string(),
                                flag_names: FLAG_NAMES.to_string(),
                            };
                            let _ = BOOTSTRAP.set(plan);
                            linker::game_activity::set_bootstrap(Some(run_bootstrap));
                            println!("  bootstrapTheApp installed");
                        } else {
                            println!("  bootstrapTheApp NOT installed (CORDIAL_NO_BOOTSTRAP)");
                        }

                        // **Before `initializeNativeCode`, not after it.**
                        //
                        // This sat beside `set_display_size` and ran 153 log
                        // lines too late: the engine builds its Configuration
                        // during `initializeNativeCode`, so it had already
                        // decided night mode was off. The value was correct
                        // and arrived after the only reader.
                        //
                        // Reported as "system preferences for dark is still
                        // not making roblox dark" after the value itself was
                        // fixed -- the third ordering bug of the day, after the
                        // cache directory and the early post, all three being
                        // the right answer at the wrong moment.
                        //
                        // Read from libadwaita's style manager, so from
                        // `org.freedesktop.appearance`'s `color-scheme`. Logged
                        // because "dark mode does not work" is three different
                        // failures and this line separates them.
                        // No `libadwaita::is_initialized()` here: that call
                        // panics when GTK is not up, and at this point it is
                        // not. It was in the first version of this line and
                        // took the process down twice.
                        let dark = cordial_shell::prefers_dark();
                        println!("  uiMode: night={}", if dark { "yes" } else { "no" });
                        linker::game_activity::set_ui_mode_night(if dark { 1 } else { 0 });

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
                                        // And the framework layer, which had no
                                        // way to be told at all: the C++ setter
                                        // behind this was never `extern "C"`, so
                                        // `DisplayMetrics`, the User-Agent
                                        // resolution fields and everything else
                                        // built on it reported the compiled
                                        // 1280x720 whatever the window was
                                        // doing. Harmless at the default
                                        // resolution, which is why it survived
                                        // this long, and wrong by the whole
                                        // difference once anyone goes fullscreen.
                                        linker::game_activity::set_display_size(
                                            width as i32,
                                            height as i32,
                                        );

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

                                        // And tell the framework layer the same
                                        // thing, before anything asks it.
                                        //
                                        // `Context.getFilesDir()` was answering
                                        // from a hardcoded
                                        // `cordial/instances/default/data` -- the
                                        // layout ADR-012 replaced -- which follows
                                        // no profile and so gave every profile the
                                        // same wrong directory. Passing the value
                                        // the engine is about to be given means the
                                        // two cannot disagree.
                                        cordial_runtime::android::system::set_files_dir(
                                            std::path::Path::new(files.as_str()),
                                        );

                                        // The directory layout Android would
                                        // already have, created before the engine
                                        // looks for it.
                                        //
                                        // `RbxStorage::init` reports
                                        // `availableDiskSpace` as part of starting,
                                        // so storage asks the filesystem how much
                                        // room it has before it builds anything --
                                        // and a `statvfs` of a directory that does
                                        // not exist fails. On a real device these
                                        // directories are part of the app's private
                                        // data layout and are simply there. Under
                                        // Cordial nothing had ever created them, so
                                        // the engine stat'd a missing path and
                                        // declined to initialise, silently: a
                                        // `CORDIAL_TRACE_PATHS=1` run intercepts
                                        // 19,296 path calls and **not one** contains
                                        // `rbx-storage`. Storage was not failing, it
                                        // was never attempting.
                                        //
                                        // The list is mocktail's (Apache-2.0,
                                        // `src/libc_shim/libc_shim.cc`
                                        // `EnsureDefaultDataLayout`), which creates
                                        // exactly these and whose own tests assert
                                        // `statvfs`/`statfs` succeed on the
                                        // `rbx-storage` path. Two of them --
                                        // `appData/LocalStorage` and
                                        // `appData/OTAPatchBackups` -- were already
                                        // visible as failed opens in Cordial's own
                                        // path trace, which should have been the
                                        // clue.
                                        //
                                        // Created under the process working
                                        // directory as well as the data root,
                                        // because the engine builds some of these
                                        // paths relatively -- the same trace shows
                                        // `fopen("cache/tombstone.dat")` and
                                        // `fopen("./appData/ClientSettings/…")`
                                        // beside absolute forms of the same files.
                                        for base in [root.as_str(), "."] {
                                            for rel in [
                                                "files",
                                                "cache",
                                                "shared_prefs",
                                                "rbx-storage",
                                                "appData",
                                                "appData/LocalStorage",
                                                "appData/rbx-storage",
                                                "appData/ClientSettings",
                                                "files/appData",
                                                "files/appData/LocalStorage",
                                                "files/appData/OTAPatchBackups",
                                                "files/appData/rbx-storage",
                                                "cache/ContentProvider_2",
                                                "cache/rbx-storage",
                                                "cache/sounds",
                                                // The external-storage tree,
                                                // the three of mocktail's list
                                                // Cordial had not taken. The
                                                // engine reaches external
                                                // storage through
                                                // `nativeSetExternalDirectory`,
                                                // which Cordial answers, but
                                                // answering with a path whose
                                                // directories do not exist is
                                                // the same shape of bug as the
                                                // failed opens above: the call
                                                // succeeds and the first write
                                                // under it does not. Whether
                                                // anything writes there on a
                                                // landing-page run is not
                                                // established; these cost three
                                                // mkdirs and remove the
                                                // question.
                                                "sdcard/Android/data/com.roblox.client",
                                                "sdcard/Android/data/com.roblox.client/files",
                                                "sdcard/Android/data/com.roblox.client/cache",
                                            ] {
                                                let _ = std::fs::create_dir_all(
                                                    format!("{base}/{rel}"),
                                                );
                                            }
                                        }
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
                                        let engine_ver = engine_version(&opt.lib_dir)
                                            .unwrap_or_default();
                                        // Read by `build_user_agent` on the C++
                                        // side, which has no other route to it.
                                        if !engine_ver.is_empty() {
                                            std::env::set_var("CORDIAL_ENGINE_VERSION", &engine_ver);
                                        }
                                        if engine_ver.is_empty() {
                                            println!("  engine version not readable from libroblox.so; not setting one");
                                        } else {
                                            println!("  engine version {engine_ver} (read from the binary)");
                                        }
                                        // The preferences file. `INFERRED`: no
                                        // capture line names it, unlike the app
                                        // policy below. The path is where the
                                        // engine already writes
                                        // `GlobalBasicSettings_13.xml` of its own
                                        // accord, so this tells it the name it
                                        // had picked anyway rather than moving
                                        // anything. If it turns out to change
                                        // nothing, say so and delete it — issue
                                        // #5 asks for that answer, not for the
                                        // call.
                                        let prefs =
                                            format!("{files}/appData/GlobalBasicSettings_13.xml");
                                        let dirs2: &[(&str, &str, Vec<&str>)] = &[
                                            (
                                                // **The one difference from Sober
                                                // that is established rather than
                                                // suspected.** Sober's own log
                                                // reports
                                                //
                                                //   rbx.JNIRobloxSettings: Setting
                                                //   default app policy file:
                                                //   content/guac/defaultConfigs/
                                                //   GuacDefaultPolicy-GlobalDist.json
                                                //
                                                // and `docs/traces/` shows the real
                                                // Android client logging that exact
                                                // line. Cordial never called this,
                                                // so the engine ran with no app
                                                // policy at all.
                                                //
                                                // Relative, not absolute, because
                                                // both the capture and Sober log it
                                                // relative — it resolves under the
                                                // asset root that
                                                // `nativeSetAssetPath` sets, and
                                                // the APK carries the file at
                                                // `assets/content/guac/...`.
                                                //
                                                // GlobalDist of the three the APK
                                                // ships (CJVDist and VNGGamesDist
                                                // are the other two) because that
                                                // is the one the capture uses and
                                                // the one named in the real
                                                // client's User-Agent as
                                                // `(GlobalDist; GooglePlayStore)`.
                                                "Java_com_roblox_engine_jni_NativeSettingsInterface_nativeSetDefaultAppPolicyFile",
                                                SETTINGS,
                                                vec![
                                                    "content/guac/defaultConfigs/GuacDefaultPolicy-GlobalDist.json",
                                                ],
                                            ),
                                            (
                                                "Java_com_roblox_engine_jni_NativeSettingsInterface_nativeSetPreferencesFile",
                                                SETTINGS,
                                                vec![prefs.as_str()],
                                            ),
                                            (
                                                "Java_com_roblox_client_startup_MainGameActivity_nativeSetAssetPath",
                                                "com/roblox/client/startup/MainGameActivity",
                                                vec![assets_now.as_str()],
                                            ),
                                            (
                                                "Java_com_roblox_engine_jni_NativeSettingsInterface_nativeSetRobloxVersion",
                                                SETTINGS,
                                                // Read out of the binary by
                                                // `engine_version`. See there for
                                                // why this is no longer a literal.
                                                vec![engine_ver.as_str()],
                                            ),
                                            (
                                                // The engine fetches its own
                                                // settings from
                                                // `clientsettingscdn.roblox.com/v2/
                                                // settings-compressed/application/
                                                // <name>.zst` and was asking for
                                                // `application/.zst` -- an EMPTY
                                                // name -- then taking the 403 and
                                                // reporting `Could not fetch
                                                // settings`. It does not know what
                                                // application it is because
                                                // nothing told it. `AndroidApp` is
                                                // not a guess: it is the name
                                                // `client_settings.rs` established
                                                // by experiment, where
                                                // AndroidClient, AndroidPlayer,
                                                // AndroidClientSettings and
                                                // AndroidAppSettings all return
                                                // HTTP 400 "The application name is
                                                // invalid" and this one returns the
                                                // real document. Verified again
                                                // here: that URL with `AndroidApp`
                                                // serves 302080 bytes.
                                                // ...and the reasoning above,
                                                // which is preserved because it
                                                // is still true, belongs to a
                                                // different question. That URL
                                                // is where the *settings
                                                // document* is fetched from and
                                                // `AndroidApp` is the right
                                                // application name for it. This
                                                // call is not that. It tells the
                                                // engine which channel platform
                                                // the *application* is, and the
                                                // two got conflated.
                                                //
                                                // `GoogleAndroidApp` is what the
                                                // real app passes, read out of
                                                // the dex rather than guessed:
                                                // the literal appears twice
                                                // there and zero times in
                                                // `libroblox.so`, while
                                                // `AndroidApp` appears three
                                                // times in the engine and zero
                                                // in the dex. Two strings, two
                                                // jobs, and this one had the
                                                // other's value.
                                                //
                                                // mocktail passes
                                                // `GoogleAndroidApp` here and
                                                // reaches `RbxStorage::init`;
                                                // Cordial passed `AndroidApp`
                                                // and does not. Whether that is
                                                // why is **not** established --
                                                // see the run recorded in the
                                                // commit, which changed nothing
                                                // measurable.
                                                "Java_com_roblox_engine_jni_NativeSettingsInterface_nativeOverrideChannelPlatformName",
                                                SETTINGS,
                                                vec!["GoogleAndroidApp"],
                                            ),
                                            (
                                                "Java_com_roblox_engine_jni_NativeSettingsInterface_nativeSetRobloxChannel",
                                                SETTINGS,
                                                // "the live channel is the empty
                                                // one" was wrong. Sober's engine
                                                // log says `The channel is
                                                // production` on the same APK, and
                                                // with the empty string the engine
                                                // wrote a `channel` preference with
                                                // an empty value and logged no
                                                // `ClientRunInfo` at all.
                                                vec![std::env::var("CORDIAL_CHANNEL").unwrap_or_else(|_| "production".into()).leak()],
                                                // `nativeSetBaseUrl` is exported
                                                // and still not called. The dex
                                                // settles its prototype --
                                                // `(Ljava/lang/String;Ljava/lang/
                                                // String;)V`, which is why an
                                                // earlier one-string guess killed
                                                // the process -- but calling it
                                                // with the same origin twice makes
                                                // the engine stop considering
                                                // itself signed in: the deeplink
                                                // join then refuses with "Signing
                                                // in is required before a join can
                                                // proceed". So the second argument
                                                // is not a second copy of the
                                                // first, and until somebody knows
                                                // what it is, not calling this is
                                                // better than calling it wrong.
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

                                        // What the display can do, alongside
                                        // the device info just above -- see
                                        // `wire_refresh_rate` for what this
                                        // does and does not establish.
                                        wire_refresh_rate(lib);

                                        // The battery, once, alongside the
                                        // display facts just wired above --
                                        // see `wire_battery_reporting` for
                                        // what this does and does not
                                        // establish about it mattering.
                                        wire_battery_reporting(lib);

                                        // The content store, after the
                                        // directories above are set and before
                                        // anything asks for an asset. The engine
                                        // reports "RbxStorage is not initialized"
                                        // on every run without this.
                                        if let Some(f) = lib.symbol(
                                            "Java_com_roblox_client_LocalStorageManager_initStorageManagerNativeV3",
                                        ) {
                                            match linker::game_activity::init_storage_manager(
                                                f, &files, &cache,
                                            ) {
                                                Ok(()) => println!("  storage manager initialised"),
                                                Err(e) => println!("  initStorageManagerNativeV3 failed: {e}"),
                                            }
                                        } else {
                                            println!("  initStorageManagerNativeV3 not exported");
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

                                        // `setWebviewUserAgent`, told the
                                        // same string `InitParams.userAgent`
                                        // just above was built with --
                                        // `cordial_runtime::webview::user_agent`
                                        // reads it back out of
                                        // `native/init_params.cpp`'s
                                        // `build_user_agent` rather than
                                        // recomputing it, so the engine and
                                        // the desktop web view
                                        // (`cordial_shell::webview::open`,
                                        // fed the same string through
                                        // `to_shell_request`) cannot disagree
                                        // about what this client claims to
                                        // be. `getWebViewUserAgent()V` on
                                        // `NativeGLJavaInterface` is the
                                        // engine's own *request* for this
                                        // value (`native/android_classes.cpp`,
                                        // still unanswered there, correctly —
                                        // see that hook's own doc) and this
                                        // call does not wait for it: nothing
                                        // established that request fires
                                        // before a window is asked to open,
                                        // and answering here, once, beside
                                        // every other `NativeGLInterface` call
                                        // this file already makes, cannot be
                                        // too late for it.
                                        match cordial_runtime::webview::user_agent() {
                                            None => println!(
                                                "  webview: could not read the User-Agent back from \
                                                 native/init_params.cpp; not calling setWebviewUserAgent"
                                            ),
                                            Some(ua) => match lib.symbol(
                                                "Java_com_roblox_engine_jni_NativeGLInterface_setWebviewUserAgent",
                                            ) {
                                                None => println!(
                                                    "  webview: setWebviewUserAgent not exported by this build"
                                                ),
                                                Some(f) => match linker::game_activity::call_static_strings(
                                                    f,
                                                    "com/roblox/engine/jni/NativeGLInterface",
                                                    &[ua.as_str()],
                                                ) {
                                                    Ok(()) => println!("  webview: setWebviewUserAgent ok"),
                                                    Err(e) => println!(
                                                        "  webview: setWebviewUserAgent failed: {e}"
                                                    ),
                                                },
                                            },
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

                                        // `ILocalStorageHandlerCore.setPlatformImpl`.
                                        //
                                        // This was skipped by default for as
                                        // long as it existed, because it
                                        // crashed the process: the call
                                        // returned cleanly and then the
                                        // engine's djinni glue threw
                                        // `djinni (djinni_support.cpp:529):
                                        // weakRef` thirteen times and the
                                        // process died on SIGTRAP, exit 133.
                                        // Both the old comment here and
                                        // docs/analysis/flag-init.md §39
                                        // blamed libjnivm's `NewWeakGlobalRef`
                                        // handing back a null weak reference.
                                        // **Both were wrong.** A trace run
                                        // with a print inside
                                        // `NewWeakGlobalRef` shows it is never
                                        // called once. djinni does not use JNI
                                        // weak references here at all -- it
                                        // constructs a real
                                        // `java.lang.ref.WeakReference` object,
                                        // which libjnivm had no implementation
                                        // for, so the constructor was an
                                        // invented stub returning null and
                                        // every later call asserted on it.
                                        // `native/local_storage.cpp` now
                                        // answers `WeakReference` and
                                        // `System.identityHashCode`, and
                                        // `native/android_classes.cpp` no
                                        // longer leaves `jnivm::Object` mapped
                                        // to `android/app/Application`, which
                                        // was corrupting the signature every
                                        // such hook is registered under. §40
                                        // records the measurement.
                                        //
                                        // Three runs with the call made and
                                        // three with it skipped, same build,
                                        // separate profile roots: exit 0 and
                                        // zero djinni exceptions either way,
                                        // and the engine's own
                                        // `FLog::LocalStorageHandler`
                                        // `Not available on the current
                                        // platform` warning appears twice per
                                        // run when it is skipped and not at
                                        // all when it is made. That warning
                                        // disappearing is the only direct
                                        // evidence the engine accepted the
                                        // implementation, and it is why this
                                        // is now unconditional.
                                        //
                                        // It does **not** produce an
                                        // `rbx-storage.db`. The engine still
                                        // reports `DFLog::RbxmFileManager`
                                        // `LocalStorageManager is not
                                        // available` twice a run either way,
                                        // and that is a different class from
                                        // the interface this hands over -- see
                                        // §40's closing note, which also
                                        // records that the engine never asks
                                        // libjnivm for `LocalStorageManager`
                                        // at all in a full trace.
                                        match lib.symbol(
                                            "Java_com_roblox_protocols_localstorageplatforminterface_generated_ILocalStorageHandlerCore_setPlatformImpl",
                                        ) {
                                            None => println!("  setPlatformImpl not exported"),
                                            Some(f) => match local_storage_set_platform_impl(f) {
                                                Ok(()) => println!("  setPlatformImpl ok"),
                                                Err(e) => {
                                                    println!("  setPlatformImpl failed: {e}")
                                                }
                                            },
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

                                        // `NativeInputInterface.nativeUpdateScreenOrientation(I)V`.
                                        // docs/analysis/flag-init.md §16: the
                                        // one call mocktail makes between
                                        // `initializeNativeCode` and the
                                        // settings handshake that Cordial
                                        // did not. Cordial already knows the
                                        // window size and, from it, whether
                                        // the window is landscape -- the same
                                        // comparison `Configuration::Create`
                                        // in init_params.cpp already makes
                                        // for `getResources().getConfiguration()`,
                                        // so this tells the engine the same
                                        // thing through its own dedicated
                                        // entry point rather than leaving it
                                        // to infer the answer from a class it
                                        // may not read this early.
                                        if let Some(f) = lib.symbol(
                                            "Java_com_roblox_engine_jni_NativeInputInterface_nativeUpdateScreenOrientation",
                                        ) {
                                            match update_screen_orientation(f, width, height) {
                                                Ok(()) => println!("  screen orientation set"),
                                                Err(e) => println!(
                                                    "  nativeUpdateScreenOrientation failed: {e}"
                                                ),
                                            }
                                        } else {
                                            println!("  nativeUpdateScreenOrientation not exported");
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
                                        // Only when bootstrapTheApp has not
                                        // already delivered. Running both meant
                                        // three registered flag providers on one
                                        // launch -- Cordial logged `Registered
                                        // Flag Provider ID from Java:` 0, 1 and 2
                                        // where Sober logs 0 and nothing else.
                                        // Whether repeated registration harms
                                        // anything is not established; matching
                                        // the real client costs nothing, and an
                                        // unnecessary difference on the path
                                        // under investigation is worth removing.
                                        let already = BOOTSTRAP_RAN
                                            .load(std::sync::atomic::Ordering::SeqCst)
                                            || std::env::var_os("CORDIAL_LATE_SETTINGS").is_some();
                                        if already {
                                            println!("  settings and flags already delivered by bootstrapTheApp");
                                        }
                                        if let Some(f) = lib
                                            .symbol(
                                                "Java_com_roblox_engine_jni_NativeGLInterface_nativeInitClientSettings",
                                            )
                                            .filter(|_| !already)
                                        {
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
                                            if let Some(f) = lib
                                                .symbol(
                                                    "Java_com_roblox_engine_jni_NativeGLInterface_nativePostClientSettingsLoadedInitialization3",
                                                )
                                                .filter(|_| !already)
                                            {
                                                match linker::game_activity::post_client_settings_loaded(f) {
                                                    Ok(()) => println!(
                                                        "  postClientSettingsLoadedInitialization3 ok"
                                                    ),
                                                    Err(e) => println!(
                                                        "  postClientSettingsLoadedInitialization3 failed: {e}"
                                                    ),
                                                }
                                            }

                                        // `CORDIAL_GLOBAL_INIT_EARLY=1` moves the
                                        // globals ahead of the settings
                                        // handshake, which is where mocktail
                                        // puts them.
                                        //
                                        // The order below -- globals *after*
                                        // settings -- came from disassembling the
                                        // ActivityNativeMain chain, and AGENTS.md
                                        // records nine consecutive conclusions
                                        // drawn that way being wrong. The reason
                                        // to doubt this one specifically is §9's
                                        // captured stack: the failure reporter is
                                        // reached through `nativeGameGlobalInit`,
                                        // so the call Cordial makes last is on the
                                        // path that announces the verdict.
                                        //
                                        // Off by default until it is shown to
                                        // change something. Shipping an inference
                                        // as a default is a mistake this file has
                                        // already made once.
                                        let globals_early =
                                            std::env::var_os("CORDIAL_GLOBAL_INIT_EARLY").is_some();
                                        if globals_early {
                                            call_globals(&lib, "early");
                                        }

                                        // What the engine actually holds, asked
                                        // rather than inferred.
                                        //
                                        // `CORDIAL_FLOG_PROBE=Name,Name` reads
                                        // each `FLog<Name>` back through
                                        // `nativeGetFInt`. It exists because
                                        // pushing values in and reading the log
                                        // gave contradictory answers: setting
                                        // `FLogNativeDM` silenced that channel
                                        // at every value tried while the same
                                        // mechanism raised `FLogAppShellReporter`
                                        // from nothing to fourteen lines. The
                                        // sentinel separates "set to 0" from
                                        // "not a registered flag", which the log
                                        // cannot do. flag-init.md §22.
                                        if let (Some(probe), Some(f)) = (
                                            std::env::var("CORDIAL_FLOG_PROBE").ok(),
                                            lib.symbol(
                                                "Java_com_roblox_client_flags_FlagJniInterface_nativeGetFInt",
                                            ),
                                        ) {
                                            const ABSENT: i32 = -424242;
                                            for name in probe.split(',').filter(|n| !n.is_empty()) {
                                                let full = if name.starts_with("FLog")
                                                    || name.starts_with("DFLog")
                                                    || name.starts_with("FInt")
                                                    || name.starts_with("DFInt")
                                                {
                                                    name.to_string()
                                                } else {
                                                    format!("FLog{name}")
                                                };
                                                match linker::game_activity::get_fint(
                                                    f, &full, ABSENT,
                                                ) {
                                                    Ok(v) if v == ABSENT => {
                                                        println!("  flog probe: {full} = <not a registered flag>")
                                                    }
                                                    Ok(v) => println!("  flog probe: {full} = {v}"),
                                                    Err(e) => {
                                                        println!("  flog probe: {full} failed: {e}")
                                                    }
                                                }
                                            }
                                        }

                                        // Flags before anything else asks for
                                        // them: bootstrapTheApp's whole job is to
                                        // reach this, and the engine reports
                                        // onFlagsFailed without it.
                                        if let Some(f) = lib
                                            .symbol(
                                                "Java_com_roblox_client_flags_FlagJniInterface_nativeInitializeNativeFlags",
                                            )
                                            .filter(|_| !already)
                                        {
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

                                        // The in-experience web window's
                                        // protocol, read out of the engine
                                        // rather than guessed at. Account
                                        // settings and Robux both open one of
                                        // these, and with nobody answering they
                                        // do nothing at all -- no window, no
                                        // error, no log line.
                                        //
                                        // Reading only. Every name below is a
                                        // getter returning a constant the engine
                                        // already holds, so this changes no
                                        // state; what it produces is the
                                        // vocabulary the receiving half will
                                        // need, which is not yet written because
                                        // the message transport has not been
                                        // traced. See crates/cordial-runtime/
                                        // src/webview.rs for why that half is
                                        // absent rather than stubbed.
                                        {
                                            let v = cordial_runtime::webview::read_vocabulary(
                                                |name| lib.symbol(name),
                                            );
                                            cordial_runtime::webview::report(&v);
                                        }

                                        // The transport for that vocabulary:
                                        // `MessageBus.getMessageId` and
                                        // `MessageBus.doSubscribeRaw`, the two
                                        // natives `openWindow` needs. Resolved
                                        // and reported only — see
                                        // crates/cordial-runtime/src/webview.rs
                                        // for why `getMessageId` is not called
                                        // from here.
                                        {
                                            let n = cordial_runtime::webview::find_bus_natives(
                                                |name| lib.symbol(name),
                                            );
                                            cordial_runtime::webview::report_bus_natives(&n);
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

                                            // The six gamepad natives, resolved
                                            // together and stored together.
                                            //
                                            // Reported once here rather than at
                                            // first use, and named individually
                                            // when any is missing, because the
                                            // failure this guards against is
                                            // silent by construction: a build
                                            // exporting the event natives but
                                            // not the registration ones would
                                            // take every button event and have
                                            // been told nothing about the
                                            // device they belong to.
                                            // `set_gamepad_natives` stores none
                                            // of them in that case, so the
                                            // gap is here in the log rather
                                            // than in a player's hands.
                                            {
                                                let sym = |n: &str| lib.symbol(n).unwrap_or(std::ptr::null_mut());
                                                let missing = cordial_runtime::android::input::set_gamepad_natives(
                                                    sym("Java_com_roblox_engine_jni_NativeInputInterface_nativeGamepadConnectEventWithGamepadType"),
                                                    sym("Java_com_roblox_engine_jni_NativeInputInterface_nativeGamepadDisconnectEvent"),
                                                    sym("Java_com_roblox_engine_jni_NativeInputInterface_nativeGamepadButtonEvent"),
                                                    sym("Java_com_roblox_engine_jni_NativeInputInterface_nativeGamepadAxisEvent"),
                                                    sym("Java_com_roblox_engine_jni_NativeInputInterface_nativeSetGamepadSupportedKeyWithGamepadType"),
                                                    sym("Java_com_roblox_engine_jni_NativeInputInterface_nativeSetGamepadSupportedMotionWithGamepadType"),
                                                );
                                                if missing.is_empty() {
                                                    println!("  input: gamepad natives resolved (all six)");
                                                } else {
                                                    println!(
                                                        "  input: gamepad disabled; not exported: {}",
                                                        missing.join(", ")
                                                    );
                                                }
                                            }

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
                                            // The finger counterpart to
                                            // nativePassMouseButton, and a
                                            // native Cordial had never called
                                            // because it had never had a
                                            // `wl_touch` to call it from. Its
                                            // descriptor `(IFFIII)V` is read
                                            // out of this build's dex; what
                                            // its three action values mean is
                                            // still INFERRED -- see
                                            // `input::TOUCH_DOWN`.
                                            let pi = lib.symbol(
                                                "Java_com_roblox_engine_jni_NativeInputInterface_nativePassInput",
                                            ).unwrap_or(std::ptr::null_mut());
                                            cordial_runtime::android::input::set_pass_input_native(pi);
                                            println!(
                                                "  input: nativePassInput {}",
                                                if pi.is_null() {
                                                    "NOT exported; touch reaches AGDK only"
                                                } else {
                                                    "resolved"
                                                }
                                            );
                                            // The other native on this
                                            // interface Cordial reads rather
                                            // than writes: where the focused
                                            // text box is. `showKeyboard`
                                            // volunteers the same thing and is
                                            // preferred, but it volunteers it
                                            // before a modal has laid out --
                                            // see `sync_text_overlay`.
                                            let gt = lib.symbol(
                                                "Java_com_roblox_engine_jni_NativeGLInterface_nativeGetTextBoxInfo",
                                            ).unwrap_or(std::ptr::null_mut());
                                            cordial_runtime::android::input::set_textbox_info_native(gt);
                                            println!(
                                                "  input: nativeGetTextBoxInfo {}",
                                                if gt.is_null() {
                                                    "NOT exported; a box focused with no geometry falls back to a placed bar"
                                                } else {
                                                    "resolved"
                                                }
                                            );
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

                                        // Globals before the app bridge:
                                        // StartLuaAppDM without them crashes on a
                                        // null JNIEnv the engine expects the
                                        // globals init to have stored. Skipped
                                        // when they were already run early.
                                        if !globals_early {
                                            call_globals(&lib, "late");
                                        }

                                        // EXPERIMENTAL, `CORDIAL_POST_BEFORE_BRIDGE=<ms>`:
                                        // the post-settings call here, with the
                                        // bridge held back for <ms> afterwards.
                                        //
                                        // All three working captures on this
                                        // machine run
                                        // `nativePostClientSettingsLoadedInitialization3`
                                        // and then `RbxStorage::init` 4-14 ms
                                        // later on another thread, and only then
                                        // `nativeAppBridgeV2Init`
                                        // (docs/analysis/flag-init.md §45.4).
                                        // Cordial's *effective* post is the one
                                        // after the surface handoff, seconds
                                        // after the bridge. This is the only
                                        // site that is both after the four
                                        // directory setters, the storage
                                        // manager and the init params -- all of
                                        // which the engine plainly wants before
                                        // it can build a store -- and before the
                                        // bridge.
                                        //
                                        // The delay exists because
                                        // `RbxStorage::init` runs on a different
                                        // thread than the post call in every
                                        // working capture, so going straight on
                                        // to the bridge would race it.
                                        //
                                        // **It works and it does not help**, and
                                        // both halves are worth keeping. The
                                        // call here logs -- `2.183367` against a
                                        // bridge at `2.725852` in one run and
                                        // `1.632804` against `2.176352` in
                                        // another -- so these are the first two
                                        // Cordial logs on this machine in which
                                        // `postClientSettingsLoadedInitialization3`
                                        // precedes `nativeAppBridgeV2Init`, the
                                        // ordering §45.4 found in all three
                                        // working captures and in none of 106
                                        // Cordial runs. Neither produced a
                                        // store. With `CORDIAL_EARLY_DIRS=off`
                                        // to isolate it, the shape the working
                                        // captures share is reproducible and
                                        // buys nothing; the directories are what
                                        // the store was waiting for. See §46.
                                        //
                                        // Note that the whole
                                        // `[FLog::ClientRunInfo]` block follows
                                        // it here as it does on Android, except
                                        // that `The base url is` prints empty --
                                        // the post body runs before Cordial has
                                        // told the engine its base URL, which is
                                        // a difference nobody has chased.
                                        if let Some(ms) = std::env::var("CORDIAL_POST_BEFORE_BRIDGE")
                                            .ok()
                                            .and_then(|v| v.parse::<u64>().ok())
                                        {
                                            match lib.symbol("Java_com_roblox_engine_jni_NativeGLInterface_nativePostClientSettingsLoadedInitialization3") {
                                                None => println!("  pre-bridge post: not exported"),
                                                Some(f) => match linker::game_activity::post_client_settings_loaded(f) {
                                                    Ok(()) => println!("  pre-bridge post: postClientSettingsLoadedInitialization3 ok"),
                                                    Err(e) => println!("  pre-bridge post failed: {e}"),
                                                },
                                            }
                                            if ms > 0 {
                                                println!("  pre-bridge post: holding the bridge for {ms} ms");
                                                std::thread::sleep(std::time::Duration::from_millis(ms));
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

                                        // The §11.7 experiment, kept because it
                                        // has a result and somebody will want to
                                        // re-run it: the handshake in Sober's
                                        // position, after the bridge.
                                        //
                                        // **It never gets here.** With the
                                        // handshake moved out of
                                        // `initializeNativeCode` the engine takes
                                        // a SIGSEGV before the app bridge is
                                        // reached -- twice out of two, against a
                                        // default run and a `CORDIAL_NO_BOOTSTRAP=1`
                                        // run in the same session, neither of
                                        // which crashes and both of which reach
                                        // the bridge. So Cordial cannot simply
                                        // adopt Sober's ordering: Sober's engine
                                        // sits idle for 2.05s waiting for the
                                        // Kotlin activity to hand it settings,
                                        // and Cordial, driving the natives
                                        // directly, has already advanced past the
                                        // point where they can arrive.
                                        if std::env::var_os("CORDIAL_LATE_SETTINGS").is_some() {
                                            println!("  late settings: delivering after the app bridge");
                                            run_bootstrap();
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

                                        // **`CORDIAL_BRIDGE_DELAY_MS`: hold the
                                        // bridge back before starting the Lua
                                        // app.**
                                        //
                                        // Measured, and it is the reason this
                                        // exists rather than a guess. Across
                                        // thirty launches with the engine's own
                                        // log timestamped, **a run that freezes
                                        // reaches every startup milestone
                                        // earlier than one that does not** --
                                        // `StartLuaAppDM` at a median of 0.490s
                                        // against 0.665s, `Lua app running
                                        // status ... true` at 0.625 against
                                        // 0.896, `sync cookies from engine` at
                                        // 1.237 against 1.558. Consistently
                                        // faster, at every mark, in the same
                                        // direction.
                                        //
                                        // Two other results point the same way.
                                        // Running the machine deliberately busy
                                        // during startup made the freeze *less*
                                        // likely, not more -- one in fifteen
                                        // against five in fifteen -- and the
                                        // person who reported the bug works
                                        // around it by giving the client input
                                        // while it loads, which is another way
                                        // of slowing that window down.
                                        //
                                        // So the shape is a race that is lost by
                                        // arriving too early, and the crudest
                                        // possible test of that is to arrive
                                        // later on purpose.
                                        if let Ok(ms) = std::env::var("CORDIAL_BRIDGE_DELAY_MS") {
                                            if let Ok(ms) = ms.parse::<u64>() {
                                                println!("  holding the bridge back {ms}ms before StartLuaAppDM");
                                                std::thread::sleep(std::time::Duration::from_millis(ms));
                                            }
                                        }
                                        if std::env::var_os("CORDIAL_SKIP_LUA_DM").is_none() {
                                        if let Some(f) = lib.symbol(
                                            "Java_com_roblox_engine_jni_NativeGLInterface_nativeAppBridgeStartLuaAppDM",
                                        ) {
                                            match linker::game_activity::appbridge_call_bare(f) {
                                                Ok(()) => println!("  Lua app DataModel started"),
                                                Err(e) => println!("  StartLuaAppDM failed: {e}"),
                                            }
                                            // The same call, kept for the pump's
                                            // startup watchdog. A third of
                                            // launches leave the engine parked
                                            // having started the Lua app once
                                            // where a healthy run starts it
                                            // twice; the watchdog asks again.
                                            // See `looper::RECOVERY_MAX_PRESENTS`
                                            // for why that cannot fire on a
                                            // client which ever drew properly.
                                            //
                                            // The address is carried as a
                                            // `usize` because a raw pointer is
                                            // not `Send`, and this closure is
                                            // read from the pump thread. It
                                            // points into `libroblox.so`, which
                                            // this process never unloads.
                                            let addr = f as usize;
                                            println!("  startup recovery armed");
                                            let _ = cordial_runtime::android::looper::STARTUP_RECOVERY
                                                .set(Box::new(move || {
                                                    linker::game_activity::appbridge_call_bare(
                                                        addr as *mut std::ffi::c_void,
                                                    )
                                                }));
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

                                        report_disk(&data_root, DiskMoment::BeforeLaunch);
                                        match linker::game_activity::start(
                                            handle, width, height, format,
                                        ) {
                                            Ok(()) => {
                                                println!("  surface handed to the engine");

                                                // `CORDIAL_LATE_POST_MS=2000`
                                                // repeats the post-settings call
                                                // here, seconds after the
                                                // bootstrap already made it.
                                                //
                                                // On Android this native is what
                                                // raises the block Cordial has
                                                // never produced -- ClientRunInfo,
                                                // the QoS handler, Mimalloc,
                                                // IxpStorageManager, the tombstone
                                                // read, and RbxStorage::init all
                                                // follow it within 25 ms. Sober
                                                // makes the call at 3.067s, about
                                                // 1.7 seconds after its engine is
                                                // alive. Cordial makes it inside
                                                // `bootstrapTheApp`, before the
                                                // engine has opened its own log,
                                                // and the block never appears.
                                                //
                                                // Every ordering tried so far moved
                                                // the settings call and this one
                                                // together, so "the handshake is
                                                // too early" and "this call is too
                                                // early" have never been separated.
                                                // Settings stay where they are, in
                                                // the bootstrap, so the flags still
                                                // load; only this moves.
                                                // On by default now, because it
                                                // is what finally produces
                                                // `gameActivity_onFlagsLoaded`.
                                                // `CORDIAL_LATE_POST_MS=off`
                                                // restores the old behaviour as a
                                                // control.
                                                //
                                                // 250 ms rather than none: at 0 the
                                                // run reaches `Flags-Not-Received=0`,
                                                // better than any delay tried, and
                                                // then segfaults. Something here is
                                                // still racing and the delay hides
                                                // it rather than fixing it. Said
                                                // plainly so the next person does not
                                                // read 250 as a tuned value -- it is
                                                // the smallest number tried that did
                                                // not crash, and the race underneath
                                                // is unfinished work.
                                                let late_post = std::env::var("CORDIAL_LATE_POST_MS")
                                                    .ok()
                                                    .map_or(Some(250), |v| {
                                                        if v == "off" { None } else { v.parse::<u64>().ok() }
                                                    });
                                                if let Some(ms) = late_post {
                                                    std::thread::sleep(
                                                        std::time::Duration::from_millis(ms),
                                                    );
                                                    // Deliver the settings again
                                                    // here, immediately before the
                                                    // post call, so the app-provided
                                                    // document is the freshest thing
                                                    // the engine has when the block
                                                    // runs.
                                                    //
                                                    // Sober's `flagLoaded` arrives
                                                    // from the app handing settings
                                                    // over; Cordial's now arrives
                                                    // from the engine fetching them
                                                    // itself inside
                                                    // `bootstrapTheApp_`. Both end
                                                    // with `continueAfterFlagsLoaded_`
                                                    // and only Sober's is followed by
                                                    // `RbxStorage::init [INIT] user:
                                                    // flagLoaded`, so the two routes
                                                    // may not be equivalent to
                                                    // whatever asks for storage.
                                                    // **Tested and it changes
                                                    // nothing**, so it is off by
                                                    // default and kept only as the
                                                    // record of a disproved theory:
                                                    // with the app delivering the
                                                    // document again here, against a
                                                    // control without it, storage is
                                                    // absent either way. The two
                                                    // routes to `flagLoaded` are not
                                                    // what distinguishes Sober.
                                                    // `CORDIAL_LATE_SETTINGS_TOO=1`
                                                    // turns it back on.
                                                    if std::env::var_os("CORDIAL_LATE_SETTINGS_TOO").is_some() {
                                                        if let Some(sf) = lib.symbol("Java_com_roblox_engine_jni_NativeGLInterface_nativeInitClientSettings") {
                                                            let doc = cordial_runtime::client_settings::load(
                                                                opt.client_settings.as_deref(),
                                                            )
                                                            .unwrap_or_default();
                                                            match linker::game_activity::init_client_settings(sf, &doc, "", "") {
                                                                Ok(code) => println!("  late settings ({} bytes) -> {code}", doc.len()),
                                                                Err(e) => println!("  late settings failed: {e}"),
                                                            }
                                                        }
                                                    }
                                                    match lib.symbol("Java_com_roblox_engine_jni_NativeGLInterface_nativePostClientSettingsLoadedInitialization3") {
                                                        None => println!("  late post: not exported"),
                                                        Some(f) => match linker::game_activity::post_client_settings_loaded(f) {
                                                            Ok(()) => println!("  late post: postClientSettingsLoadedInitialization3 ok (after {ms} ms)"),
                                                            Err(e) => println!("  late post failed: {e}"),
                                                        },
                                                    }

                                                    // `CORDIAL_LATE_RETRY=1` asks
                                                    // the engine to run its init
                                                    // again, here, once the block
                                                    // above has actually produced
                                                    // something.
                                                    //
                                                    // `RbxStorage::init` is logged
                                                    // on Android with
                                                    // `user: flagLoaded` -- the
                                                    // flags-loaded event is what
                                                    // asks for it, and by this
                                                    // point Cordial's verdict has
                                                    // already come back failed
                                                    // twice. Cordial does call
                                                    // `nativeRetryInit`, but early,
                                                    // before any of this ran. This
                                                    // is the same call at the only
                                                    // moment where the state it
                                                    // would retry against is
                                                    // different.
                                                    if std::env::var("CORDIAL_LATE_RETRY").map_or(true, |v| v != "off") {
                                                        match lib.symbol("Java_com_roblox_client_startup_MainGameActivity_nativeRetryInit") {
                                                            None => println!("  late retry: not exported"),
                                                            Some(f) => match linker::game_activity::appbridge_call_bare(f) {
                                                                Ok(()) => println!("  late retry: nativeRetryInit ok"),
                                                                Err(e) => println!("  late retry failed: {e}"),
                                                            },
                                                        }
                                                    }
                                                }
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

                                                // ADR-026's core bus, from the
                                                // client rather than from a
                                                // plugin. Until this line
                                                // existed the bus had no
                                                // producer under `cordial-run`
                                                // at all: `discord-presence`
                                                // called `lifecycle.subscribe`,
                                                // was told `ok`, and then waited
                                                // for a `cordial/client.launch`
                                                // nothing in the shipping client
                                                // could publish.
                                                //
                                                // Published here rather than at
                                                // the top of `main`, where the
                                                // client was actually asked to
                                                // start, because plugins are
                                                // deliberately started late --
                                                // see the comment above -- and a
                                                // publish before `start_all` has
                                                // nobody to reach. This is the
                                                // first moment the fact can be
                                                // told, not the moment it became
                                                // true.
                                                //
                                                // The profile's *name*, not its
                                                // path. A plugin may reasonably
                                                // key what it remembers by which
                                                // profile is running; it has no
                                                // business learning where the
                                                // user's home directory is, and
                                                // ADR-007's rule that a plugin
                                                // gets the effect rather than the
                                                // channel reads the same way here.
                                                cordial_runtime::plugin_host::publish_core(
                                                    cordial_plugins::core_events::CLIENT_LAUNCH,
                                                    serde_json::json!({
                                                        "profile": cordial_runtime::profile::active()
                                                            .file_name()
                                                            .map(|n| n.to_string_lossy().into_owned()),
                                                    }),
                                                );

                                                // `engine_ver`, read once
                                                // during bring-up, rather than
                                                // `engine_version(&opt.lib_dir)`
                                                // again here. This line called
                                                // it a second time, on the
                                                // reasoning that
                                                // `build_user_agent` reads the
                                                // version by the same function
                                                // -- which is not true.
                                                // `native/init_params.cpp:372`
                                                // reads the environment
                                                // variable set from the very
                                                // read above. Nothing reads
                                                // `libroblox.so` twice, and the
                                                // second read was not free:
                                                // `cordial_update::engine::scan`
                                                // has no early exit, because it
                                                // must reach EOF to notice a
                                                // second, differing candidate.
                                                // That is a byte walk over the
                                                // whole 118 MB library, on the
                                                // main thread, at the moment
                                                // the engine is up and waiting
                                                // for its first pump.
                                                //
                                                // Empty means it was not
                                                // readable, and then nothing is
                                                // published at all -- inventing
                                                // a version is exactly the bug
                                                // `engine_version`'s own
                                                // comment records.
                                                if engine_ver.is_empty() {
                                                    println!(
                                                        "  plugins: engine version not readable, so cordial/engine.version is not published"
                                                    );
                                                } else {
                                                    cordial_runtime::plugin_host::publish_core(
                                                        cordial_plugins::core_events::ENGINE_VERSION,
                                                        serde_json::json!({ "version": engine_ver }),
                                                    );
                                                }

                                                // Subscribe to the engine's
                                                // openWindow before the pump
                                                // starts, the same point
                                                // `android::clipboard::arm`
                                                // is called from inside that
                                                // pump: the message bus has
                                                // to exist first, and by now
                                                // the app bridge has started.
                                                // This module cannot reach
                                                // `looper::pump` to add
                                                // itself there (off limits
                                                // for this change), so it is
                                                // called from here instead,
                                                // one call earlier than
                                                // clipboard's but after the
                                                // same precondition holds.
                                                cordial_runtime::webview::arm(|name| lib.symbol(name));
                                                // Same precondition, same
                                                // moment: the bus exists, so
                                                // the outbound half of
                                                // linking can bind its
                                                // request handler. Without
                                                // this, clicking Terms,
                                                // Privacy or any external
                                                // link does nothing at all --
                                                // the engine issues the
                                                // request and no one answers.
                                                cordial_runtime::linking::arm(|name| lib.symbol(name));
                                                install_webview_presenter();

                                                // A dev-only trigger, in the same family as
                                                // `CORDIAL_TRACE_PATHS`: off by default, out of the
                                                // ordinary path, and read exactly once, here, after
                                                // the presenter above is installed and before the
                                                // pump this whole feature depends on starts.
                                                //
                                                // It exists because `openWindow` needs a real click
                                                // in signed-in UI and AGENTS.md's "Two practical
                                                // cautions" rules out faking that click at the
                                                // compositor -- so there was no way to see whether a
                                                // web window survives the engine's Vulkan swapchain
                                                // without driving Cordial's own presenter directly.
                                                // `webview::dev_trigger_open_window` is that: it
                                                // skips the engine and the message bus, but not the
                                                // policy check or the presenter itself, so what opens
                                                // (or is refused) here is exactly what a real click
                                                // would have produced for the same URL.
                                                if let Ok(url) = std::env::var("CORDIAL_WEBVIEW_TEST") {
                                                    println!(
                                                        "  webview: CORDIAL_WEBVIEW_TEST set; synthesising an \
                                                         openWindow request for {url}"
                                                    );
                                                    cordial_runtime::webview::dev_trigger_open_window(url);
                                                }

                                                cordial_runtime::android::looper::pump(
                                                    std::time::Duration::from_secs(secs),
                                                    Some(handle),
                                                );
                                                // Ungated, unlike the block below: this
                                                // is the one line somebody debugging a
                                                // mysterious crash needs, and hiding it
                                                // behind a switch they do not know about
                                                // is how it stayed unsaid.
                                                report_disk(&data_root, DiskMoment::AfterExit);
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

                                                    // The instrument ADR-021
                                                    // is built around: one
                                                    // cold launch plus one
                                                    // game join is the
                                                    // ground-truth list of
                                                    // every asset this build
                                                    // actually reads, which
                                                    // is what both orphan
                                                    // signals are diffed
                                                    // against. Written at the
                                                    // end because the set is
                                                    // only complete then, and
                                                    // never fatal -- a run
                                                    // that produced a client
                                                    // is not a failed run
                                                    // because a log could not
                                                    // be saved.
                                                    let trace =
                                                        cordial_runtime::android::asset::trace_path();
                                                    match cordial_runtime::android::asset::write_trace(&trace) {
                                                        Ok(n) => println!(
                                                            "\n  {n} distinct assets requested -> {}",
                                                            trace.display()
                                                        ),
                                                        Err(e) => println!(
                                                            "  asset trace not written ({}): {e}",
                                                            trace.display()
                                                        ),
                                                    }
                                                    for line in
                                                        cordial_runtime::android::asset::shadow_report()
                                                    {
                                                        println!("    overlay: {line}");
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

    // Whether Roblox's own storage came up, which the engine's `RbxStorage::init
    // [INIT]` line would say if it were not logged before the log file exists.
    // The same `files` directory the tree above was created under.
    cordial_runtime::storage::report(std::path::Path::new(&format!(
        "{}/files",
        std::env::var("CORDIAL_FILES_DIR")
            .unwrap_or_else(|_| format!("{}/data", cordial_runtime::profile::active().display()))
    )));

    stubs::report();

    // Everything the engine asked for that Cordial could not answer, in one
    // table: JNI classes and methods libjnivm never had, libc stubs, AGDK
    // natives called while unregistered, and framework calls that returned
    // something invented. Printed and written beside the engine's own logs,
    // because the question after a failure is "what did we fail to tell it"
    // and the answer used to be spread across four kinds of line.
    cordial_runtime::unimplemented::report();

    // The last thing any plugin is told, and the one event that has to be
    // waited for. Delivery is asynchronous by design, so a publish followed by
    // `_exit` is a race the exit wins -- the pump thread is still holding the
    // event when the process goes. `flush_core_events` is bounded for the
    // opposite reason: a plugin that stopped reading must not be able to hold
    // up Cordial's exit.
    //
    // Every path that gets a plugin running comes through here: `start_all` is
    // called after the engine has the surface, and every `return` in this
    // function is upstream of that, so there is no exit that skips this except
    // a crash.
    cordial_runtime::plugin_host::publish_core(
        cordial_plugins::core_events::CLIENT_SHUTDOWN,
        serde_json::Value::Null,
    );
    // 500 ms for the whole flush rather than 500 ms per plugin, which is what
    // a loop over `Pump::flush` would cost: it takes a fresh deadline each
    // call, and the number of plugins is the user's choice, so the promise
    // above would have been one the code could not keep. Named, because "a
    // plugin" sends whoever reads it to look at all of them.
    for id in cordial_runtime::plugin_host::flush_core_events(std::time::Duration::from_millis(500))
    {
        println!("  plugin {id}: still had queued core events when the 500 ms shutdown budget ran out; exiting without it");
    }
    // Said once, at the end, because an event that nobody counts is the silent
    // failure the bounded queue exists to avoid becoming. Which of the two
    // reasons it was comes from the pump rather than from a guess: this line
    // used to say "its queue was full" for every one, including a plugin that
    // had crashed, which points a reader at the queue depth and the plugin's
    // read loop for a plugin that was not slow at all.
    for u in cordial_runtime::plugin_host::undelivered_core_events() {
        let why = if u.plugin_gone {
            "it had stopped reading"
        } else {
            "its queue was full"
        };
        println!("  plugin {}: {} core event(s) never delivered, {why}", u.id, u.events);
    }

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

/// The store behind `native/local_storage.cpp`'s `PlatformLocalStorageHandler`
/// -- `ILocalStorageHandlerCore.setPlatformImpl`'s per-user, per-key secure
/// values, which is a different thing from `RbxStorage` (the content cache)
/// and from `LocalStorageManager`'s own `initStorageManagerNativeV3`. See that
/// file's header for the full account of what the interface is and how it was
/// confirmed; this module is the half of it the task that added it could not
/// put in `secrets.rs`.
///
/// **Why this is not a third `secrets::Kind`.** `secrets.rs` is this project's
/// settled answer for where a per-profile secret goes -- the desktop Secret
/// Service first, an announced `0600` file second, never a reason startup
/// fails -- and the right move would have been to add a variant and call it.
/// Two things stopped that. First, the task this module was written under
/// left `secrets.rs` off limits to edit, on the reasoning that a file several
/// agents have been relying on as a fixed reference should not move under
/// them mid-session. Second, and the reason a variant would not have been
/// enough even without that restriction: `Kind::load`/`save` hold exactly one
/// document per profile, and what this interface asks for is an arbitrary
/// number of small values keyed by an account id *and* a name the engine
/// picks — `getSecureValue`, `setSecureValueForUser`, `deleteUserValues`, all
/// of them shaped around a key that is not fixed at compile time the way
/// `"cookies"` and `"identity"` are. So this reuses `secrets::active()` --
/// the same environment variable, the same keyring-vs-file-vs-none decision,
/// decided once and shared with the cookie jar and the identity mirror rather
/// than asked a second time -- and carries its own small read/write/remove
/// against the same `org.freedesktop.secrets` interface under its own schema,
/// because that half of `secrets.rs` is `HashMap<String,String>`-shaped for
/// one document and cannot be reused as-is for many.
///
/// **The same restraint on printing.** Nothing below prints a stored value or
/// a user id at any verbosity, matching `secrets.rs`'s own header and
/// AGENTS.md's rule that this project's stubs answer honestly rather than
/// pretending. Key *names* are printed, the same way `secrets.rs` prints
/// `"cookies"`/`"identity"` -- they identify which field failed, not whose
/// account or what the field held.
///
/// **Why a fresh connection per call rather than `secrets.rs`'s worker
/// thread.** That thread exists because `secrets.rs` is called on a flush
/// cadence and a stuck keyring daemon must not freeze whichever thread asks
/// next. Local storage's calls are a handful of account-scoped values, not a
/// periodic save, so the simpler shape here — connect, ask, time out, drop
/// the connection — is enough: a wedged daemon leaks one thread for the one
/// call that hit it rather than jamming every later call behind a single
/// stuck worker the way a shared thread would.
mod local_storage_secrets {
    use std::collections::HashMap;
    use std::ffi::CStr;
    use std::io::Write as _;
    use std::os::raw::{c_char, c_int, c_longlong};
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    use std::path::PathBuf;
    use std::time::Duration;

    use cordial_runtime::secrets::{self, Store};
    use zbus::blocking::{Connection, Proxy};
    use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

    const SERVICE: &str = "org.freedesktop.secrets";
    const SERVICE_PATH: &str = "/org/freedesktop/secrets";
    const IFACE_SERVICE: &str = "org.freedesktop.Secret.Service";
    const IFACE_ITEM: &str = "org.freedesktop.Secret.Item";
    /// A schema of its own, distinct from `secrets.rs`'s `org.cordial.Session`
    /// -- so `secret-tool`/Seahorse show the two families separately, and so a
    /// search for one can never turn up an item that belongs to the other.
    const SCHEMA: &str = "org.cordial.LocalStorageSecureValue";
    const CONTENT_TYPE: &str = "text/plain; charset=utf8";
    const CALL_TIMEOUT: Duration = Duration::from_secs(3);
    const FILE_NAME: &str = "local-storage-secrets.json";

    fn profile_dir() -> PathBuf {
        cordial_runtime::profile::active()
    }

    /// Keyed by profile path (never by name — see `secrets.rs`'s own
    /// `attributes()` for why two profiles both called `default` must not
    /// share an item) plus the account id and, for a single value, the name
    /// the engine gave it. Omitting `key` widens a search to every value held
    /// for that account, which `delete_user` below relies on.
    fn attrs(user_id: i64, key: Option<&str>) -> HashMap<String, String> {
        let mut m = HashMap::from([
            ("xdg:schema".to_string(), SCHEMA.to_string()),
            ("application".to_string(), "cordial".to_string()),
            ("profile".to_string(), profile_dir().display().to_string()),
            ("user".to_string(), user_id.to_string()),
        ]);
        if let Some(k) = key {
            m.insert("key".to_string(), k.to_string());
        }
        m
    }

    fn with_timeout<T: Send + 'static>(
        f: impl FnOnce() -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Result<T, String>>(1);
        if std::thread::Builder::new()
            .name("cordial-ls-secret".to_string())
            .spawn(move || {
                let _ = tx.send(f());
            })
            .is_err()
        {
            return Err("could not start a worker thread".to_string());
        }
        rx.recv_timeout(CALL_TIMEOUT).unwrap_or_else(|_| {
            Err(format!(
                "the secret service did not answer within {} seconds",
                CALL_TIMEOUT.as_secs()
            ))
        })
    }

    fn session() -> Result<(Connection, Proxy<'static>, OwnedObjectPath, OwnedObjectPath), String> {
        let conn = Connection::session().map_err(|_| "there is no session bus".to_string())?;
        let service = Proxy::new_owned(conn.clone(), SERVICE, SERVICE_PATH, IFACE_SERVICE)
            .map_err(|e| format!("the secret service could not be addressed ({e})"))?;
        let (_output, open_session): (OwnedValue, OwnedObjectPath) = service
            .call("OpenSession", &("plain", Value::from("")))
            .map_err(|_| "there is no secret service on the session bus".to_string())?;
        let collection: OwnedObjectPath = service
            .call("ReadAlias", &("default",))
            .map_err(|e| format!("the secret service has no default collection ({e})"))?;
        if collection.as_str() == "/" {
            return Err("the secret service has no default collection".to_string());
        }
        Ok((conn, service, open_session, collection))
    }

    fn proxy(conn: &Connection, path: &OwnedObjectPath, iface: &'static str) -> Result<Proxy<'static>, String> {
        Proxy::new_owned(conn.clone(), SERVICE, path.clone().into_inner(), iface)
            .map_err(|e| format!("{path} could not be addressed ({e})"))
    }

    fn keyring_read(request_attrs: HashMap<String, String>) -> Result<Option<String>, String> {
        with_timeout(move || {
            let (conn, service, item_session, _collection) = session()?;
            let (unlocked, _locked): (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) = service
                .call("SearchItems", &(request_attrs,))
                .map_err(|e| format!("the keyring could not be searched ({e})"))?;
            let Some(item) = unlocked.into_iter().next() else {
                return Ok(None);
            };
            let (_session, _parameters, value, _content): (
                OwnedObjectPath,
                Vec<u8>,
                Vec<u8>,
                String,
            ) = proxy(&conn, &item, IFACE_ITEM)?
                .call("GetSecret", &(&item_session,))
                .map_err(|e| format!("the stored value could not be read ({e})"))?;
            String::from_utf8(value)
                .map(Some)
                .map_err(|_| "the stored value is not text".to_string())
        })
    }

    fn keyring_write(
        request_attrs: HashMap<String, String>,
        label: String,
        body: String,
    ) -> Result<(), String> {
        with_timeout(move || {
            let (conn, _service, item_session, collection) = session()?;
            let mut properties: HashMap<&str, Value<'_>> = HashMap::new();
            properties.insert("org.freedesktop.Secret.Item.Label", Value::from(label.as_str()));
            properties.insert(
                "org.freedesktop.Secret.Item.Attributes",
                Value::from(request_attrs),
            );
            let secret = (item_session, Vec::<u8>::new(), body.into_bytes(), CONTENT_TYPE);
            let (_item, prompt): (OwnedObjectPath, OwnedObjectPath) =
                proxy(&conn, &collection, "org.freedesktop.Secret.Collection")?
                    .call("CreateItem", &(properties, secret, true))
                    .map_err(|e| format!("the value could not be stored ({e})"))?;
            if prompt.as_str() != "/" {
                return Err("storing the value would have needed a prompt".to_string());
            }
            Ok(())
        })
    }

    fn keyring_remove(request_attrs: HashMap<String, String>) -> Result<(), String> {
        with_timeout(move || {
            let (conn, service, _item_session, _collection) = session()?;
            let (unlocked, _locked): (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) = service
                .call("SearchItems", &(request_attrs,))
                .map_err(|e| format!("the keyring could not be searched ({e})"))?;
            for item in unlocked {
                let _prompt: OwnedObjectPath = proxy(&conn, &item, IFACE_ITEM)?
                    .call("Delete", &())
                    .map_err(|e| format!("a stored value could not be removed ({e})"))?;
            }
            Ok(())
        })
    }

    // -----------------------------------------------------------------
    // The file backend: one JSON document per profile rather than one file
    // per value, for the same reason `secrets.rs`'s file store is one
    // document rather than one file per cookie — a directory full of
    // ad-hoc-named files in a profile is a worse audit surface than one
    // named store, and `write_file` below is the same temp-then-rename
    // shape `secrets.rs`'s `write_private` uses, for the same reason: a
    // reader must see the old body or the new one, never half of either.
    // -----------------------------------------------------------------

    type FileMap = HashMap<String, HashMap<String, String>>;

    fn file_path() -> PathBuf {
        profile_dir().join(FILE_NAME)
    }

    fn file_load() -> FileMap {
        std::fs::read_to_string(file_path())
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn file_save(map: &FileMap) -> std::io::Result<()> {
        let final_path = file_path();
        let tmp = profile_dir().join(format!("{FILE_NAME}.new"));
        let body = serde_json::to_string(map).map_err(std::io::Error::other)?;

        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, &final_path)
    }

    // -----------------------------------------------------------------
    // The three operations, dispatched on the same `Store` cookies and
    // identity already settled on this launch.
    // -----------------------------------------------------------------

    fn get(user_id: i64, key: &str) -> Option<String> {
        match secrets::active() {
            Store::None => None,
            Store::File => file_load().get(&user_id.to_string()).and_then(|m| m.get(key)).cloned(),
            Store::Keyring => match keyring_read(attrs(user_id, Some(key))) {
                Ok(v) => v,
                Err(why) => {
                    println!("  [local-storage] {key}: not read back ({why})");
                    None
                }
            },
        }
    }

    fn set(user_id: i64, key: &str, value: &str) -> bool {
        match secrets::active() {
            // Matches secrets.rs's own `Store::None` save: accepted and
            // discarded rather than refused, so a user who opted out of
            // storage entirely is not additionally punished with a JNI
            // `false` the engine has no way to explain to anyone.
            Store::None => true,
            Store::File => {
                let mut map = file_load();
                map.entry(user_id.to_string())
                    .or_default()
                    .insert(key.to_string(), value.to_string());
                match file_save(&map) {
                    Ok(()) => true,
                    Err(e) => {
                        println!("  [local-storage] {key}: not saved ({e})");
                        false
                    }
                }
            }
            Store::Keyring => {
                let label = format!(
                    "Cordial: Roblox local storage ({key}) for profile {:?}",
                    profile_dir().file_name().map(|n| n.to_string_lossy().into_owned())
                );
                match keyring_write(attrs(user_id, Some(key)), label, value.to_string()) {
                    Ok(()) => true,
                    Err(why) => {
                        println!("  [local-storage] {key}: not saved ({why})");
                        false
                    }
                }
            }
        }
    }

    fn delete(user_id: i64, key: &str) -> bool {
        match secrets::active() {
            Store::None => true,
            Store::File => {
                let mut map = file_load();
                if let Some(m) = map.get_mut(&user_id.to_string()) {
                    m.remove(key);
                }
                match file_save(&map) {
                    Ok(()) => true,
                    Err(e) => {
                        println!("  [local-storage] {key}: not removed ({e})");
                        false
                    }
                }
            }
            Store::Keyring => match keyring_remove(attrs(user_id, Some(key))) {
                Ok(()) => true,
                Err(why) => {
                    println!("  [local-storage] {key}: not removed ({why})");
                    false
                }
            },
        }
    }

    fn delete_user(user_id: i64) -> bool {
        match secrets::active() {
            Store::None => true,
            Store::File => {
                let mut map = file_load();
                map.remove(&user_id.to_string());
                match file_save(&map) {
                    Ok(()) => true,
                    Err(e) => {
                        println!("  [local-storage] account values: not removed ({e})");
                        false
                    }
                }
            }
            // No "key" attribute: every item this profile holds for the
            // account, not one value of it.
            Store::Keyring => match keyring_remove(attrs(user_id, None)) {
                Ok(()) => true,
                Err(why) => {
                    println!("  [local-storage] account values: not removed ({why})");
                    false
                }
            },
        }
    }

    // -----------------------------------------------------------------
    // The C boundary. `native/local_storage.cpp` declares these four
    // directly against these symbol names — see that file's header for why
    // there is no generated binding for them.
    // -----------------------------------------------------------------

    unsafe fn borrow_str<'a>(p: *const c_char) -> Option<&'a str> {
        if p.is_null() {
            return None;
        }
        // SAFETY: the caller (native/local_storage.cpp) passes a
        // NUL-terminated buffer it owns for the duration of this call.
        unsafe { CStr::from_ptr(p) }.to_str().ok()
    }

    /// Returns `0` on an ordinary call, whether or not anything was found;
    /// `*found` and `*out_len` carry the actual answer. `-1` means the call
    /// itself could not be made (a bad key, a null buffer) rather than
    /// anything about whether a value exists.
    #[no_mangle]
    pub extern "C" fn cordial_local_storage_get(
        user_id: c_longlong,
        key: *const c_char,
        out: *mut c_char,
        out_cap: usize,
        found: *mut c_int,
        out_len: *mut usize,
    ) -> c_int {
        // SAFETY: `key` is a NUL-terminated C string owned by the caller for
        // the duration of this call; `out`/`found`/`out_len` are live
        // buffers the caller sized and will read back afterwards.
        let Some(key) = (unsafe { borrow_str(key) }) else {
            return -1;
        };
        if out.is_null() || found.is_null() || out_len.is_null() {
            return -1;
        }
        let value = get(user_id as i64, key);
        // SAFETY: pointers were just checked non-null; `out` has `out_cap`
        // bytes per the caller's own contract in local_storage.cpp.
        unsafe {
            match value {
                None => {
                    *found = 0;
                    *out_len = 0;
                }
                Some(v) => {
                    let bytes = v.as_bytes();
                    // `>=` rather than `>`: a byte of the cap is reserved for
                    // the NUL the C++ side reads the string through.
                    if bytes.len() >= out_cap {
                        println!(
                            "  [local-storage] {key}: {} bytes does not fit the platform \
                             buffer; treated as absent rather than truncated",
                            bytes.len()
                        );
                        *found = 0;
                        *out_len = bytes.len();
                    } else {
                        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out as *mut u8, bytes.len());
                        *out.add(bytes.len()) = 0;
                        *found = 1;
                        *out_len = bytes.len();
                    }
                }
            }
        }
        0
    }

    #[no_mangle]
    pub extern "C" fn cordial_local_storage_set(
        user_id: c_longlong,
        key: *const c_char,
        value: *const c_char,
        value_len: usize,
    ) -> c_int {
        // SAFETY: as above; `value` points to `value_len` bytes the caller
        // owns for the duration of this call.
        let Some(key) = (unsafe { borrow_str(key) }) else {
            return -1;
        };
        if value.is_null() {
            return -1;
        }
        let bytes = unsafe { std::slice::from_raw_parts(value as *const u8, value_len) };
        let Ok(value) = std::str::from_utf8(bytes) else {
            println!("  [local-storage] {key}: value is not UTF-8; refused rather than stored");
            return -1;
        };
        if set(user_id as i64, key, value) { 0 } else { -1 }
    }

    #[no_mangle]
    pub extern "C" fn cordial_local_storage_delete(user_id: c_longlong, key: *const c_char) -> c_int {
        let Some(key) = (unsafe { borrow_str(key) }) else {
            return -1;
        };
        if delete(user_id as i64, key) { 0 } else { -1 }
    }

    #[no_mangle]
    pub extern "C" fn cordial_local_storage_delete_user(user_id: c_longlong) -> c_int {
        if delete_user(user_id as i64) { 0 } else { -1 }
    }
}

#[cfg(test)]
mod disk_tests {
    use super::*;
    use std::path::Path;

    /// **A disk that is fine says nothing.** A warning on every launch is a
    /// warning nobody reads, which is how the real one would be missed.
    #[test]
    fn a_healthy_disk_is_silent() {
        let p = Path::new("/tmp");
        assert!(disk_warning(50 * 1024 * 1024 * 1024, DiskMoment::BeforeLaunch, p).is_none());
        assert!(disk_warning(50 * 1024 * 1024 * 1024, DiskMoment::AfterExit, p).is_none());
    }

    /// Before a launch there is still time to act, so a merely low disk is
    /// worth a line. Afterwards it is not: the run already happened and
    /// "you were a bit low" explains nothing.
    #[test]
    fn low_warns_only_before_a_launch() {
        let p = Path::new("/tmp");
        let low = 1024 * 1024 * 1024;
        assert!(disk_warning(low, DiskMoment::BeforeLaunch, p).is_some());
        assert!(disk_warning(low, DiskMoment::AfterExit, p).is_none());
    }

    /// **The case this exists for.** A client that stopped on a nearly-full
    /// disk has to say so, because the engine will not: it does not report a
    /// failed write, it fails somewhere with no relationship to the cause.
    #[test]
    fn a_nearly_full_disk_is_named_at_both_moments() {
        let p = Path::new("/home/someone/.local/share/cordial/profiles/x/files");
        let critical = 100 * 1024 * 1024;

        let before = disk_warning(critical, DiskMoment::BeforeLaunch, p).expect("a warning");
        assert!(before.contains("100 MB free"), "{before}");
        assert!(before.contains("asset cache"), "{before}");

        let after = disk_warning(critical, DiskMoment::AfterExit, p).expect("a warning");
        assert!(after.contains("the client stopped"), "{after}");
        assert!(after.contains("does not report a full disk"), "{after}");
        // And it names where, because a machine has more than one filesystem
        // and the profile is not always on the one the user is watching.
        assert!(after.contains("profiles/x/files"), "{after}");
    }
}
