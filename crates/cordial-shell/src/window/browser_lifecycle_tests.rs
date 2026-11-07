use super::*;
use std::cell::Cell;
use std::os::unix::fs::PermissionsExt;

static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn exit_ui_preserves_crashes_visible_picker_other_clients_and_pending_lookup() {
    // Given every reason the shell must remain available after a child exits.
    let cases = [
        (true, 0, false, false, ExitPresentation::Crash),
        (false, 0, true, false, ExitPresentation::Keep),
        (false, 1, false, false, ExitPresentation::Keep),
        (false, 0, false, true, ExitPresentation::Keep),
    ];

    // When the exit presentation is selected.
    for (crashed, remaining, visible, pending, expected) in cases {
        // Then each reason prevents hidden-launcher teardown.
        assert_eq!(
            exit_presentation(crashed, remaining, visible, pending),
            expected
        );
    }
    assert_eq!(
        exit_presentation(false, 0, false, false),
        ExitPresentation::Close
    );
}

#[test]
#[ignore = "requires a GTK display; run alone with isolated XDG directories"]
fn automatic_browser_launch_starts_and_last_clean_client_closes_hidden_launcher() {
    // Given a hidden production shell and a real child watched by GLib.
    let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let _profile_root = crate::PROFILE_ROOT_ENV
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    adw::init().unwrap();
    let root = tempfile::tempdir().unwrap();
    let loader = root.path().join("cordial-run");
    let launched_args = root.path().join("launched-args");
    std::fs::write(
        &loader,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\nexit 0\n",
            launched_args.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&loader, std::fs::Permissions::from_mode(0o755)).unwrap();
    let apk = root.path().join("base.apk");
    // Beside the APK, which is where `install::locate` looks for an unpacked
    // engine now that Settings has no engine-directory row to point elsewhere.
    let lib_dir = root.path().join("lib").join(cordial_update::apk::HOST_ABI);
    std::fs::write(&apk, []).unwrap();
    std::fs::create_dir_all(&lib_dir).unwrap();
    std::fs::write(lib_dir.join(install::LIBRARY), []).unwrap();

    let old_path = std::env::var_os("PATH");
    unsafe { std::env::set_var("PATH", root.path()); }
    unsafe { std::env::set_var("CORDIAL_PROFILE_ROOT", root.path().join("profiles")); }

    let app = adw::Application::builder()
        .application_id("org.cordial.BrowserLifecycleTest")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let mut shell_config = ShellConfig::default();
    shell_config.profile = "clean-exit".into();
    shell_config.roblox.apk = Some(apk);
    let config = Rc::new(RefCell::new(shell_config));
    let config_path = Rc::new(root.path().join("shell.json"));
    let requested = "roblox-player:1+launchmode:play+placelauncherurl:x";

    let started = Rc::new(Cell::new(false));
    {
        let config = config.clone();
        let config_path = config_path.clone();
        let started = started.clone();
        app.connect_activate(move |app| {
            let shell = build(app, config.clone(), config_path.clone(), false);
            shell.join.queue(requested.into());
            started.set(WidgetExt::activate_action(&shell.window, "win.launch", None).is_ok());
        });
    }
    let timed_out = Rc::new(Cell::new(false));
    let timeout = {
        let app = app.clone();
        let timed_out = timed_out.clone();
        glib::timeout_add_local_once(Duration::from_secs(3), move || {
            timed_out.set(true);
            app.quit();
        })
    };

    // When automatic account selection invokes the normal launch action.
    app.run_with_args(&["cordial-browser-lifecycle-test"]);

    if !timed_out.get() {
        timeout.remove();
    }
    match old_path {
        Some(path) => unsafe { std::env::set_var("PATH", path); },
        None => unsafe { std::env::remove_var("PATH"); },
    }
    unsafe { std::env::remove_var("CORDIAL_PROFILE_ROOT"); }

    // Then the join reached the child and its clean exit did not strand the shell.
    assert!(started.get());
    assert!(
        !timed_out.get(),
        "clean child exit left its hidden launcher registered"
    );
    let args = std::fs::read_to_string(launched_args).unwrap();
    assert!(args.lines().any(|line| line == "--join-url"), "{args}");
    assert!(args.lines().any(|line| line == requested), "{args}");
}

#[test]
#[ignore = "requires a GTK display; run alone with isolated XDG directories"]
fn manual_launch_entry_invalidates_lookup_without_consuming_join() {
    // Given a hidden shell with a browser lookup represented by its checking banner.
    let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let _profile_root = crate::PROFILE_ROOT_ENV
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    adw::init().unwrap();
    let root = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("CORDIAL_PROFILE_ROOT", root.path().join("profiles")); }
    let app = adw::Application::builder()
        .application_id("org.cordial.ManualLaunchLifecycleTest")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.register(None::<&gtk::gio::Cancellable>).unwrap();
    let config = Rc::new(RefCell::new(ShellConfig::default()));
    config.borrow_mut().profile = "last-used".into();
    config.borrow_mut().roblox.apk = Some(root.path().join("missing.apk"));
    let shell = build(&app, config, Rc::new(root.path().join("shell.json")), false);
    let requested = "roblox-player:1+launchmode:play+gameinfo:FAKE+placelauncherurl:x";
    let task = shell
        .queue_with_lookup(requested.into(), |_| Some("browser-account".into()))
        .unwrap();

    // When manual launch is requested and fails before spawning a child.
    WidgetExt::activate_action(&shell.window, "win.launch", None).unwrap();
    glib::MainContext::default().block_on(task).unwrap();

    // Then its late lookup cannot select or launch, while retry data remains.
    assert_eq!(shell.config.borrow().profile, "last-used");
    assert_eq!(
        shell.join.peek().as_deref(),
        Some("roblox-player:1+launchmode:play+placelauncherurl:x")
    );
    shell.window.close();
    unsafe { std::env::remove_var("CORDIAL_PROFILE_ROOT"); }
}

#[test]
#[ignore = "requires a GTK display; run alone with isolated XDG directories"]
fn changed_profile_after_account_resolution_falls_back_before_spawn() {
    // Given account B matched exact files before its old client exits.
    let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let _profile_root = crate::PROFILE_ROOT_ENV
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    adw::init().unwrap();
    let root = tempfile::tempdir().unwrap();
    let browser = root.path().join("profiles/browser");
    std::fs::create_dir_all(&browser).unwrap();
    std::fs::write(browser.join("identity"), r#"{"schema":1,"userId":34}"#).unwrap();
    std::fs::write(
        browser.join("cookies"),
        ".roblox.com\t.ROBLOSECURITY=SESSION-B\n",
    )
    .unwrap();
    let matched = crate::browser_account::snapshot_for_test(
        "browser",
        &browser,
        cordial_shell::secrets::Store::File,
    )
    .unwrap();

    let loader = root.path().join("cordial-run");
    std::fs::write(&loader, "#!/bin/sh\n/usr/bin/sleep 2\n").unwrap();
    std::fs::set_permissions(&loader, std::fs::Permissions::from_mode(0o755)).unwrap();
    let apk = root.path().join("base.apk");
    // Beside the APK, which is where `install::locate` looks for an unpacked
    // engine now that Settings has no engine-directory row to point elsewhere.
    let lib_dir = root.path().join("lib").join(cordial_update::apk::HOST_ABI);
    std::fs::write(&apk, []).unwrap();
    std::fs::create_dir_all(&lib_dir).unwrap();
    std::fs::write(lib_dir.join(install::LIBRARY), []).unwrap();
    let old_path = std::env::var_os("PATH");
    unsafe { std::env::set_var("PATH", root.path()); }
    unsafe { std::env::set_var("CORDIAL_PROFILE_ROOT", root.path().join("profiles")); }

    let app = adw::Application::builder()
        .application_id("org.cordial.ProfileSnapshotTest")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.register(None::<&gtk::gio::Cancellable>).unwrap();
    let window = adw::Window::new();
    window.set_application(Some(&app));
    let toasts = adw::ToastOverlay::new();
    let join = PendingJoin::new();
    window.set_content(Some(join.banner()));
    let mut shell_config = ShellConfig::default();
    shell_config.profile = "last-used".into();
    shell_config.roblox.apk = Some(apk);
    let config = Rc::new(RefCell::new(shell_config));
    let lifecycle = LaunchLifecycle::default();
    let actions = gtk::gio::SimpleActionGroup::new();
    let launch = gtk::gio::SimpleAction::new("launch", None);
    {
        let window = window.clone();
        let toasts = toasts.clone();
        let config = config.clone();
        let join = join.clone();
        launch.connect_activate(move |_, _| {
            join.invalidate_lookup(&config.borrow().profile);
            activate_roblox(
                &window.clone().upcast(),
                &toasts,
                &config,
                &join,
                &lifecycle,
                Mode::Phone,
            );
        });
    }
    actions.add_action(&launch);
    window.insert_action_group("win", Some(&actions));
    let shell = Shell {
        window: window.clone(),
        join: join.clone(),
        config: config.clone(),
        config_path: Rc::new(root.path().join("shell.json")),
        refresh_profiles: Rc::new({
            let browser = browser.clone();
            move || {
                std::fs::write(browser.join("identity"), r#"{"schema":1,"userId":12}"#).unwrap();
                std::fs::write(
                    browser.join("cookies"),
                    ".roblox.com\t.ROBLOSECURITY=SESSION-A\n",
                )
                .unwrap();
            }
        }),
    };

    // When selection succeeds, then account A replaces B before the action claims it.
    let task = shell
        .queue_with_lookup(
            "roblox-player:1+launchmode:play+gameinfo:FAKE+placelauncherurl:x".into(),
            |_| Some(matched.into()),
        )
        .unwrap();
    glib::MainContext::default().block_on(task).unwrap();

    // Then no stale automatic launch inherited the profile lock or consumed the join.
    assert!(cordial_shell::profile::acquire("browser").is_ok());
    assert!(window.is_visible());
    assert_eq!(
        join.peek().as_deref(),
        Some("roblox-player:1+launchmode:play+placelauncherurl:x")
    );
    window.close();
    match old_path {
        Some(path) => unsafe { std::env::set_var("PATH", path); },
        None => unsafe { std::env::remove_var("PATH"); },
    }
    unsafe { std::env::remove_var("CORDIAL_PROFILE_ROOT"); }
}
