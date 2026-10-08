use super::*;
use std::cell::Cell;

#[test]
#[ignore = "requires a GTK display; run alone"]
fn browser_account_launches_matching_profile_through_window_action() {
    // Given the actual shell widgets with a recording player-launch boundary.
    adw::init().unwrap();
    let root = tempfile::tempdir().unwrap();
    let config = Rc::new(RefCell::new(ShellConfig::default()));
    config.borrow_mut().profile = "last-used".into();
    let window = adw::Window::new();
    let join = PendingJoin::new();
    window.set_content(Some(join.banner()));
    let launches = Rc::new(Cell::new(0));
    let refreshes = Rc::new(Cell::new(0));
    let actions = adw::gtk::gio::SimpleActionGroup::new();
    let launch = adw::gtk::gio::SimpleAction::new("launch", None);
    let main_loop = glib::MainLoop::new(None, false);
    {
        let launches = launches.clone();
        let config = config.clone();
        let join = join.clone();
        let main_loop = main_loop.clone();
        launch.connect_activate(move |_, _| {
            assert_eq!(config.borrow().profile, "browser-account");
            assert_eq!(
                join.peek().as_deref(),
                Some("roblox-player:1+launchmode:play+placelauncherurl:x")
            );
            launches.set(launches.get() + 1);
            join.clear();
            main_loop.quit();
        });
    }
    actions.add_action(&launch);
    window.insert_action_group("win", Some(&actions));
    let shell = Shell {
        window,
        join,
        config,
        config_path: Rc::new(root.path().join("shell.json")),
        refresh_profiles: {
            let refreshes = refreshes.clone();
            Rc::new(move || refreshes.set(refreshes.get() + 1))
        },
    };
    let timed_out = Rc::new(Cell::new(false));
    let timeout = {
        let main_loop = main_loop.clone();
        let timed_out = timed_out.clone();
        glib::timeout_add_local_once(std::time::Duration::from_secs(5), move || {
            timed_out.set(true);
            main_loop.quit();
        })
    };
    // When a browser ticket resolves on the worker thread.
    let _task = shell.queue_with_lookup(
        "roblox-player:1+launchmode:play+gameinfo:FAKE+placelauncherurl:x".into(),
        |_| Some("browser-account".into()),
    );
    main_loop.run();
    // Then the regular action runs exactly once with the consumed ticket absent.
    assert!(!timed_out.get());
    timeout.remove();
    assert_eq!(launches.get(), 1);
    assert_eq!(refreshes.get(), 1);
    assert!(shell.join.peek().is_none());
    assert!(!shell.window.is_visible());
    shell.window.close();
}

/// The name the header bar's profile button is showing: its label, which is the
/// profile the next launch will use.
fn shown_profile(button: &adw::gtk::MenuButton) -> Option<String> {
    button.child().and_downcast::<adw::gtk::Label>().map(|label| label.text().to_string())
}

#[test]
#[ignore = "requires a GTK display; run alone"]
fn matched_profile_is_shown_when_automatic_launch_waits_for_retry() {
    // Given two real profiles and the actual launcher profile button showing the last-used one.
    adw::init().unwrap();
    let _guard = crate::PROFILE_ROOT_ENV
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let root = tempfile::tempdir().unwrap();
    std::env::set_var("CORDIAL_PROFILE_ROOT", root.path());
    std::fs::create_dir(root.path().join("last-used")).unwrap();
    std::fs::create_dir(root.path().join("browser-account")).unwrap();
    let config = Rc::new(RefCell::new(ShellConfig::default()));
    config.borrow_mut().profile = "last-used".into();
    let config_path = Rc::new(root.path().join("shell.json"));
    let chooser = crate::profile_switcher::build(config.clone(), config_path.clone());
    assert_eq!(shown_profile(&chooser.button), Some("last-used".to_string()));

    let window = adw::Window::new();
    let join = PendingJoin::new();
    let launches = Rc::new(Cell::new(0));
    let shown_profiles = Rc::new(RefCell::new(Vec::new()));
    let actions = adw::gtk::gio::SimpleActionGroup::new();
    let launch = adw::gtk::gio::SimpleAction::new("launch", None);
    let main_loop = glib::MainLoop::new(None, false);
    {
        let button = chooser.button.clone();
        let launches = launches.clone();
        let shown_profiles = shown_profiles.clone();
        let join = join.clone();
        let main_loop = main_loop.clone();
        launch.connect_activate(move |_, _| {
            let shown = shown_profile(&button);
            shown_profiles.borrow_mut().push(shown);
            launches.set(launches.get() + 1);
            if launches.get() == 1 {
                main_loop.quit();
            } else {
                join.clear();
            }
        });
    }
    actions.add_action(&launch);
    window.insert_action_group("win", Some(&actions));
    let shell = Shell {
        window,
        join,
        config,
        config_path,
        refresh_profiles: chooser.refresh,
    };

    // When the account match selects a profile but the automatic launch leaves the join queued.
    let _task = shell.queue_with_lookup(
        "roblox-player:1+launchmode:play+gameinfo:FAKE+placelauncherurl:x".into(),
        |_| Some("browser-account".into()),
    );
    main_loop.run();

    // Then the button shows what a manual retry will launch, and that retry consumes the same join.
    assert_eq!(launches.get(), 1);
    assert_eq!(
        shown_profiles.borrow().as_slice(),
        &[Some("browser-account".into())]
    );
    assert!(shell.join.peek().is_some());
    shell.window.activate_action("win.launch", None).unwrap();
    assert_eq!(launches.get(), 2);
    assert_eq!(
        shown_profiles.borrow().as_slice(),
        &[
            Some("browser-account".into()),
            Some("browser-account".into())
        ]
    );
    assert!(shell.join.peek().is_none());
    shell.window.close();
}

#[test]
#[ignore = "requires a GTK display; run alone"]
fn unmatched_account_keeps_ticketless_join_for_manual_launch() {
    // Given the shell with no matching account returned by its lookup.
    let (shell, launches, _root) = fallback_shell();
    // When account resolution finishes.
    let task = shell
        .queue_with_lookup(
            "roblox-player:1+launchmode:play+gameinfo:FAKE+placelauncherurl:x".into(),
            |_| None,
        )
        .unwrap();
    glib::MainContext::default().block_on(task).unwrap();
    // Then the original choice remains, and the join waits without its ticket.
    assert_eq!(launches.get(), 0);
    assert_eq!(shell.config.borrow().profile, "last-used");
    assert_eq!(
        shell.join.peek().as_deref(),
        Some("roblox-player:1+launchmode:play+placelauncherurl:x")
    );
    assert!(shell.join.banner.is_revealed());
    shell.window.close();
}

#[test]
#[ignore = "requires a GTK display; run alone"]
fn discarded_request_cannot_launch_after_account_lookup_finishes() {
    // Given a queued lookup that would find a matching account.
    let (shell, launches, _root) = fallback_shell();
    let task = shell
        .queue_with_lookup(
            "roblox-player:1+launchmode:play+gameinfo:FAKE+placelauncherurl:x".into(),
            |_| Some("browser-account".into()),
        )
        .unwrap();
    // When the user discards it before the main loop receives the result.
    shell.join.banner.emit_by_name::<()>("button-clicked", &[]);
    glib::MainContext::default().block_on(task).unwrap();
    // Then no late launch or selection change occurs.
    assert_eq!(launches.get(), 0);
    assert_eq!(shell.config.borrow().profile, "last-used");
    assert!(shell.join.peek().is_none());
    shell.window.close();
}

#[test]
#[ignore = "requires a GTK display; run alone"]
fn newer_identical_join_invalidates_previous_account_lookup() {
    // Given an older lookup for the same place as the next request.
    let (shell, launches, _root) = fallback_shell();
    let task = shell
        .queue_with_lookup(
            "roblox-player:1+launchmode:play+gameinfo:FAKE+placelauncherurl:x".into(),
            |_| Some("browser-account".into()),
        )
        .unwrap();
    // When another click queues identical join parameters with a distinct identity.
    shell
        .join
        .queue("roblox-player:1+launchmode:play+placelauncherurl:x".into());
    glib::MainContext::default().block_on(task).unwrap();
    // Then the older result cannot select an account or launch the newer join.
    assert_eq!(launches.get(), 0);
    assert_eq!(shell.config.borrow().profile, "last-used");
    assert!(shell.join.peek().is_some());
    shell.window.close();
}

pub(super) fn fallback_shell() -> (Shell, Rc<Cell<u32>>, tempfile::TempDir) {
    adw::init().unwrap();
    let root = tempfile::tempdir().unwrap();
    let config = Rc::new(RefCell::new(ShellConfig::default()));
    config.borrow_mut().profile = "last-used".into();
    let window = adw::Window::new();
    let join = PendingJoin::new();
    window.set_content(Some(join.banner()));
    let launches = Rc::new(Cell::new(0));
    let actions = adw::gtk::gio::SimpleActionGroup::new();
    let launch = adw::gtk::gio::SimpleAction::new("launch", None);
    {
        let launches = launches.clone();
        launch.connect_activate(move |_, _| launches.set(launches.get() + 1));
    }
    actions.add_action(&launch);
    window.insert_action_group("win", Some(&actions));
    (
        Shell {
            window,
            join,
            config,
            config_path: Rc::new(root.path().join("shell.json")),
            refresh_profiles: Rc::new(|| {}),
        },
        launches,
        root,
    )
}

#[test]
#[ignore = "requires a GTK display; run alone with CORDIAL_BROWSER_ACCOUNT_ROUTING=0"]
fn disabled_routing_preserves_original_manual_launch() {
    // Given the same shell with automatic routing disabled in its environment.
    let (shell, launches, _root) = fallback_shell();
    let raw = "roblox-player:1+launchmode:play+gameinfo:FAKE+placelauncherurl:x";
    // When a browser link arrives, lookup must not run.
    let task = shell.queue_with_lookup(raw.into(), |_| panic!("routing is disabled"));
    // Then the untouched join waits for the normal launch button.
    assert!(task.is_none());
    assert_eq!(shell.join.peek().as_deref(), Some(raw));
    assert_eq!(launches.get(), 0);
    shell.window.close();
}
