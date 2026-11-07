//! What ADR-013 claims about a *running* client, checked by running one half of
//! it rather than by reading the code.
//!
//! Two things here cannot be tested from `src`: `flags::collect` migrates the
//! legacy overrides file behind a `std::sync::Once`, and `plugin_host::start_all`
//! resolves the profile through `profile::set_active`, which is a `OnceLock`.
//! Both are per-process facts, so each needs a process that has not already
//! decided them — which is what an integration test is. The two live in one
//! file, and one test, for exactly that reason: a second test in this binary
//! would be sharing both.

use std::time::{Duration, Instant};

fn wait_for(path: &std::path::Path, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if path.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("{what} never appeared at {}", path.display());
}

#[test]
fn a_launch_reads_its_flags_and_grants_from_the_profile_it_was_given() {
    if std::process::Command::new("deno").arg("--version").output().is_err() {
        eprintln!("skipping: deno is not installed");
        return;
    }

    let root = std::env::temp_dir().join(format!("cordial-profile-config-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let config = root.join("config");
    let profile = root.join("profiles/alt");
    let plugins = root.join("installed-plugins");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::create_dir_all(config.join("cordial")).unwrap();
    std::fs::create_dir_all(plugins.join("profile-demo")).unwrap();

    unsafe { std::env::set_var("XDG_CONFIG_HOME", &config); }
    unsafe { std::env::set_var("CORDIAL_PLUGIN_DIR", &plugins); }
    unsafe { std::env::remove_var("CORDIAL_FLAGS"); }
    unsafe { std::env::remove_var("CORDIAL_PLUGIN_GRANTS"); }

    // The profile arrives as one decision, the way `--profile` will hand it
    // over, and everything below is resolved from it.
    cordial_runtime::profile::set_active(profile.clone()).unwrap();

    // A user with overrides from before ADR-013, in the old global place.
    let legacy_flags = config.join("cordial/flags.json");
    std::fs::write(&legacy_flags, r#"{"FIntTaskSchedulerAutoThreadLimit":"8"}"#).unwrap();

    // An approval made in a *different* profile, and none in this one. This is
    // the control for the assertion further down: the first `start_all` must
    // refuse to start the plugin, and the only difference between the two calls
    // is which profile directory the grant is written into.
    //
    // Writing the grant to the old global location would not be a control,
    // because `start_all` migrates a legacy file into the profile before
    // reading it — a distinction that cost this test one wrong version.
    let neighbour = root.join("profiles/main");
    std::fs::create_dir_all(&neighbour).unwrap();
    std::fs::write(
        neighbour.join("plugin-grants.json"),
        r#"{"profile-demo":["settings.read","settings.write"]}"#,
    )
    .unwrap();

    std::fs::write(
        plugins.join("profile-demo/plugin.json"),
        r#"{"id":"profile-demo","name":"Profile Demo","entry":"main.ts","capabilities":["settings.read","settings.write"]}"#,
    )
    .unwrap();
    std::fs::write(
        plugins.join("profile-demo/main.ts"),
        include_str!("fixtures/profile_demo.ts"),
    )
    .unwrap();

    // The legacy overrides file moves on the first read of the user's flags,
    // and the moved values are actually in the layer that comes back — not
    // merely present on disk somewhere.
    let layers = cordial_runtime::flags::collect();
    assert!(!legacy_flags.exists(), "the legacy overrides file should have moved");
    let resolved = cordial_runtime::flags::resolve(layers);
    assert_eq!(
        resolved["FIntTaskSchedulerAutoThreadLimit"].value, "8",
        "the moved overrides must still be in effect"
    );
    assert_eq!(
        std::fs::read_to_string(profile.join("flags.json")).unwrap().trim(),
        r#"{"FIntTaskSchedulerAutoThreadLimit":"8"}"#
    );

    assert_eq!(
        cordial_runtime::plugin_host::start_all(),
        0,
        "an approval made in another profile must not start a plugin here"
    );

    // Approved here, and only here.
    std::fs::write(
        profile.join("plugin-grants.json"),
        r#"{"profile-demo":["settings.read","settings.write"]}"#,
    )
    .unwrap();
    let started = cordial_runtime::plugin_host::start_all();
    assert_eq!(started, 1, "the plugin granted in this profile should have started");

    // The plugin was handed its settings in the handshake and saved a new
    // document. Both ends of that land inside the profile.
    let saved = profile.join("plugins/profile-demo/settings.json");
    wait_for(&saved, "the plugin's settings");
    let document: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&saved).unwrap()).unwrap();
    assert_eq!(
        document["handshakeSaw"],
        serde_json::json!({}),
        "a plugin with nothing saved should be told so, not left guessing: {document}"
    );
    assert_eq!(document["launches"], 1);

    // And nothing leaked back out to the machine-wide locations.
    assert!(!config.join("cordial/plugin-grants.json").exists());
    assert!(
        !plugins.join("profile-demo/settings.json").exists(),
        "settings must not land beside the installed code"
    );

    let _ = std::fs::remove_dir_all(&root);
}
