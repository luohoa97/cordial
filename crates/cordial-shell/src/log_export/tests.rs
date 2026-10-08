use super::*;

fn identity() -> Identity {
    Identity { home: Some("/home/alice".into()), os_user: Some("alice".into()), profiles: vec!["default".into(), "AliceMain".into()] }
}

fn redactor() -> Redactor {
    Redactor::new(identity())
}

// Lines below are the shapes the repository's own tests and docs already use
// (`cookies.rs`, `deeplink.rs`, `game_log.rs`, `input.rs`); the secrets in them
// are synthetic.

#[test]
fn a_session_cookie_value_goes_in_every_shape_it_is_printed() {
    let r = redactor();
    for line in [
        ".ROBLOSECURITY=xxx; path=/",
        ".ROBLOSECURITY=super-secret-value",
        "#HttpOnly_.roblox.com\tTRUE\t/\tFALSE\t0\t.ROBLOSECURITY\tsuper-secret-value",
        r#"{"name":".ROBLOSECURITY","value":"_|WARNING:-DO-NOT-SHARE-THIS.--Sharing-this-will-allow-someone-to-log-in-as-you|_CAEaAhAB"}"#,
        "Cookie: RBXEventTrackerV2=x; .ROBLOSECURITY=super-secret-value; other=1",
        "Set-Cookie: .ROBLOSECURITY=super-secret-value; domain=.roblox.com",
        "x-csrf-token: AbCdEf123456",
        "Authorization: Bearer super-secret-value",
    ] {
        let out = r.redact_line(line);
        for secret in ["xxx", "super-secret-value", "_|WARNING", "CAEaAhAB", "AbCdEf123456"] {
            assert!(!out.contains(secret), "{secret:?} survived in {out:?} (from {line:?})");
        }
        assert!(out.contains("<redacted>"), "{out:?}");
    }
}

#[test]
fn a_launch_link_loses_its_ticket_and_keeps_its_shape() {
    let r = redactor();
    let line = "roblox-player:1+launchmode:play+gameinfo:SYNTHETIC-NOT-A-TICKET\
                +placelauncherurl:https%3A%2F%2Fassetgame.roblox.com%2Fgame%2FPlaceLauncher.ashx%3Frequest%3DRequestGame%26placeId%3D1818\
                +launchtime:1754179200000+browsertrackerid:1";
    let out = r.redact_line(line);
    assert!(!out.contains("SYNTHETIC"), "{out}");
    assert!(!out.contains("RequestGame"), "{out}");
    assert!(out.contains("launchmode:play") && out.contains("launchtime:1754179200000"), "{out}");
}

#[test]
fn an_authentication_url_keeps_its_path_and_loses_its_query() {
    let r = redactor();
    let out = r.redact_line(
        "fetching https://assetgame.roblox.com/game/PlaceLauncher.ashx?request=RequestGame&browserTrackerId=1&placeId=1818&auth=abc123 now",
    );
    assert_eq!(out, "fetching https://assetgame.roblox.com/game/PlaceLauncher.ashx?<query removed> now");
    let out = r.redact_line("GET https://bob:hunter2@example.com/v1/x#frag done");
    assert_eq!(out, "GET https://example.com/v1/x#<removed> done");
    // No query, nothing to say about it.
    assert_eq!(r.redact_line("see https://github.com/luohoa97/cordial/issues"), "see https://github.com/luohoa97/cordial/issues");
}

#[test]
fn ids_and_names_get_one_placeholder_each_across_every_source() {
    let mut r = redactor();
    let join = "[FLog::GameJoinLoadTime] Report game_join_loadtime: sid:6baeb082, clienttime:1.2, join_time:1.2011154180, referral_page:, \
                placeid:17625359962, userid:1826805362, universeid:6035872082,";
    let other = r#"profile response {"userId":1826805362,"username":"BlockyBuilder99","displayName":"Blocky Builder"}"#;
    let later = "welcome BlockyBuilder99 (1826805362) at https://users.roblox.com/v1/users/1826805362/status";
    for t in [join, other, later] {
        r.learn(t);
    }
    let a = r.redact_line(join);
    let b = r.redact_line(other);
    let c = r.redact_line(later);
    for out in [&a, &b, &c] {
        assert!(!out.contains("1826805362"), "{out}");
        assert!(!out.contains("BlockyBuilder99"), "{out}");
    }
    assert!(a.contains("userid:<user-id-1>"), "{a}");
    assert!(a.contains("placeid:17625359962"), "a place is not a person: {a}");
    assert!(b.contains(r#""userId":<user-id-1>"#) || b.contains(r#""userId":"<user-id-1>""#), "{b}");
    assert!(b.contains("<username-1>"), "{b}");
    assert_eq!(c, "welcome <username-1> (<user-id-1>) at https://users.roblox.com/v1/users/<user-id-1>/status");
}

#[test]
fn typed_text_is_withheld_but_its_length_is_kept() {
    let r = redactor();
    // `CORDIAL_TRACE_TEXT=1` without the show-passwords switch: a length only.
    let counted = "[cordial] key down keysym=0x61 text=<1 bytes, 1 chars> keycode=Some(29) focus=None";
    assert_eq!(r.redact_line(counted), counted);
    // With `CORDIAL_TRACE_TEXT_SHOW_PASSWORDS`, or the accessibility trace.
    let shown = r#"[cordial] key down keysym=0x61 text="hunter2" keycode=Some(29)"#;
    let out = r.redact_line(shown);
    assert!(!out.contains("hunter2") && out.contains(r#"text="<withheld>""#), "{out}");
    let acc = r#"[accessibility] event type=16 class="android.widget.EditText" text="my password is swordfish""#;
    assert!(!r.redact_line(acc).contains("swordfish"));
}

#[test]
fn chat_channels_and_session_narration_are_replaced_whole() {
    let r = redactor();
    assert_eq!(
        r.redact_line("12:00:00.000 [FLog::ClientChat] <BlockyBuilder99> hi everyone, my address is 1 Main St"),
        "12:00:00.000 [FLog::ClientChat] (message withheld)"
    );
    assert_eq!(r.redact_line("  [identity] signed in; saved to /home/alice/x (username 12 bytes)"), SESSION_MARKER);
    assert_eq!(r.redact_line("  [cookies] roblox.com: saved 3 domain(s), 412 bytes to /home/alice/c"), SESSION_MARKER);
    // An ordinary engine line is untouched.
    let ordinary = "[FLog::SingleSurfaceApp] leaveUGCGameInternal";
    assert_eq!(r.redact_line(ordinary), ordinary);
}

#[test]
fn the_machine_owner_and_profile_names_are_not_in_paths() {
    let r = redactor();
    let out = r.redact_line("log at /home/alice/.local/share/cordial/instances/AliceMain/data/files/appData/logs/x_last.log for alice");
    assert!(!out.contains("alice") && !out.contains("AliceMain"), "{out}");
    assert!(out.contains("~/.local/share/cordial/instances/<profile>/data"), "{out}");
    // The stock profile name is information, not identity.
    assert!(r.redact_line("/x/instances/default/data").contains("instances/default/"));
    let flat = r.redact_line("opened /var/home/bob/Projects/x");
    assert!(flat.contains("/home/<user>/"), "{flat}");
}

#[test]
fn ordinary_diagnostics_survive_the_pass() {
    let r = redactor();
    let block = "Cordial   0.11.0 (0fdbb4425)\nInstall   rpm\nRoblox    2.736.1408 (fetched by Cordial)\n\
                 System    Linux host 7.1.8-200.fc44.x86_64 x86_64 GNU/Linux\nDistro    Fedora Linux 44\nSession   wayland (GNOME)\n";
    assert_eq!(r.redact(block), block);
}

#[test]
fn engine_logs_are_the_newest_last_logs_and_a_long_one_is_cut_to_its_end() {
    let dir = tempfile::tempdir().unwrap();
    let mk = |name: &str, age: u64, body: &str| {
        let p = dir.path().join(name);
        std::fs::write(&p, body).unwrap();
        let t = std::time::SystemTime::now() - std::time::Duration::from_secs(age);
        std::fs::File::options().write(true).open(&p).unwrap().set_modified(t).unwrap();
    };
    mk("2.734.0.917_20260801T000000Z_Player_aaaaa_last.log", 300, "oldest\n");
    mk("2.734.0.917_20260802T000000Z_Player_bbbbb_last.log", 200, "middle\n");
    mk("2.734.0.917_20260803T000000Z_Player_ccccc_last.log", 100, "newest\n");
    mk("2.734.0.917_20260804T000000Z_Player_ddddd.log", 1, "still being written\n");
    mk("notes.txt", 1, "not a log\n");
    let names: Vec<String> = newest_engine_logs(dir.path())
        .iter()
        .map(|p| p.file_name().unwrap().to_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["2.734.0.917_20260803T000000Z_Player_ccccc_last.log", "2.734.0.917_20260802T000000Z_Player_bbbbb_last.log"]);

    // With nothing rotated yet, the live log is better than none.
    let only = tempfile::tempdir().unwrap();
    std::fs::write(only.path().join("x_Player_y.log"), "live\n").unwrap();
    assert_eq!(newest_engine_logs(only.path()).len(), 1);
    assert!(newest_engine_logs(&only.path().join("missing")).is_empty());

    let big = dir.path().join("big.log");
    let line = "0123456789abcdef0123456789abcdef\n";
    std::fs::write(&big, line.repeat((ENGINE_LOG_BYTES as usize / line.len()) + 100)).unwrap();
    let (text, cut) = read_tail(&big).unwrap();
    assert!(cut && text.len() as u64 <= ENGINE_LOG_BYTES && text.starts_with("0123"), "{} {cut}", text.len());
}

#[test]
fn the_coredump_note_says_how_to_look_and_never_offers_a_core() {
    let none = coredump_text(Ok(String::new()));
    assert!(none.contains("no core dump") && none.contains("coredumpctl list cordial-run"), "{none}");
    let some = coredump_text(Ok(
        "Thu 2026-10-08 10:11:12 UTC 4242 1000 1000 SIGSEGV present /usr/bin/cordial-run 12.3M\n".into(),
    ));
    assert!(some.contains("1 core dump(s)") && some.contains("SIGSEGV") && some.contains("coredumpctl info"), "{some}");
    let missing = coredump_text(Err("coredumpctl did not run".into()));
    assert!(missing.contains("could not check") && missing.contains("coredumpctl list cordial-run"), "{missing}");
    assert!(some.contains("never carries one"));
}

fn sample() -> Inputs {
    Inputs {
        diagnostics: "Cordial   0.27.0 (0814e2d)\nRoblox    2.737.0.1 (fetched by Cordial)\n".into(),
        launcher: vec![
            "10:00:00.000   shell: starting Roblox on AliceMain".into(),
            "10:00:01.000   [cookies] roblox.com: saved 3 domain(s), 412 bytes to /home/alice/x".into(),
            "10:00:02.000 [cordial] key down keysym=0x61 text=\"hunter2\" keycode=Some(29)".into(),
            "10:00:03.000 launching roblox-player:1+launchmode:play+gameinfo:SYNTHETIC-NOT-A-TICKET+launchtime:1".into(),
        ],
        client: Some((
            vec![
                ".ROBLOSECURITY=super-secret-value; path=/".into(),
                "[FLog::GameJoinLoadTime] Report game_join_loadtime: placeid:1, userid:1826805362, universeid:2,".into(),
            ],
            "/usr/bin/cordial-run --profile AliceMain --run 30".into(),
        )),
        health: vec!["10:00:30.000 [cordial] health: 1061 presents in 31s (34.2/s), 1061 total".into()],
        engine: vec![EngineLog {
            label: "phone",
            name: "2.737.0.1_20261008T000000Z_Player_abcde_last.log".into(),
            text: "[FLog::Network] connected https://apis.roblox.com/x?token=abc123\nhi BlockyBuilder99 userid:1826805362 username:BlockyBuilder99\n"
                .into(),
            cut: false,
        }],
        coredump: coredump_text(Ok(String::new())),
        identity: identity(),
    }
}

#[test]
fn the_archive_lists_what_it_should_and_nothing_secret_survives_in_any_of_it() {
    let entries = entries(&sample());
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "README.txt",
            "diagnostics.txt",
            "launcher.log",
            "client-output.log",
            "health.log",
            "engine/phone-2.737.0.1_20261008T000000Z_Player_abcde_last.log",
            "coredump.txt"
        ]
    );
    for entry in &entries {
        let text = String::from_utf8(entry.data.clone()).unwrap();
        for secret in [
            "super-secret-value",
            "SYNTHETIC-NOT-A-TICKET",
            "hunter2",
            "1826805362",
            "BlockyBuilder99",
            "abc123",
            "/home/alice",
            "AliceMain",
        ] {
            assert!(!text.contains(secret), "{secret:?} survived in {}:\n{text}", entry.name);
        }
    }
    let engine = String::from_utf8(entries[5].data.clone()).unwrap();
    assert!(engine.contains("<username-1>") && engine.contains("<user-id-1>"), "{engine}");
    let client = String::from_utf8(entries[3].data.clone()).unwrap();
    assert!(client.contains("<user-id-1>"), "the same account is the same placeholder in a different file: {client}");
    assert!(client.contains("--profile <profile>"), "{client}");
}

#[test]
fn the_zip_reads_back_with_the_same_files() {
    let entries = entries(&sample());
    let bytes = zip_bytes(&entries).unwrap();
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    assert_eq!(archive.len(), entries.len());
    let mut health = String::new();
    archive.by_name("health.log").unwrap().read_to_string(&mut health).unwrap();
    assert!(health.contains("[cordial] health: 1061 presents"));
}

#[test]
fn an_empty_session_still_makes_an_archive_that_says_why_it_is_thin() {
    let entries = entries(&Inputs { diagnostics: "Cordial   0.27.0\n".into(), ..Inputs::default() });
    let text = |n: &str| String::from_utf8(entries.iter().find(|e| e.name == n).unwrap().data.clone()).unwrap();
    assert!(text("client-output.log").contains("No client was started"));
    assert!(text("health.log").contains("No `[cordial] health:` line"));
    assert!(text("README.txt").contains("no Roblox engine log was found"));
}
