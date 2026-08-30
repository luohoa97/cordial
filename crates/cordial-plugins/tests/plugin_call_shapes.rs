//! Every method the shipped plugins call must be a method that exists.
//!
//! **This exists because `plugins/fps-flex/` did nothing at all for its entire
//! shipped life, and nothing noticed.** It called `settings.read`, which is a
//! capability name and not a method, and it sent `flags.set` a `{key, value}`
//! pair when the handler requires `{values: {...}}`. Both calls came back
//! refused, the plugin logged nothing about either, and Settings went on
//! advertising it as a built-in that changes the frame rate.
//!
//! The reason no test caught it is worth writing down, because it is a shape
//! this repository can produce again. The plugin tests in this crate drive
//! `host::Session`, and `Session` is constructed nowhere outside this crate's
//! own tests -- the host `cordial-run` actually runs is
//! `crates/cordial-runtime/src/plugin_host.rs`. So the suite was green against
//! a dispatcher no user ever reaches, which is the "verified against the
//! selector, false in the client" failure AGENTS.md records about the audio
//! backend, one crate over.
//!
//! What a text check like this can hold is the half that does not need a
//! running client: a method name that is not in the closed table, a call whose
//! shape the handler refuses, and a call the plugin's own manifest never asked
//! the capability for. It cannot prove a plugin works. The end-to-end proof is
//! a client run with the plugin enabled, and it belongs in a report, not here.
//!
//! It reads the shipped plugins as text rather than running them because
//! running them needs Deno, a profile and a grant, and a test that quietly
//! skips itself when Deno is absent is a test that reports success for having
//! done nothing.

use std::path::{Path, PathBuf};

/// `plugins/`, at the repository root.
fn plugins_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins")
}

/// Every `main.ts` under `plugins/`, with the id of the plugin it belongs to.
fn shipped() -> Vec<(String, String, PathBuf)> {
    let mut out = Vec::new();
    let dir = plugins_dir();
    let entries = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("plugins/ should be readable at {}: {e}", dir.display()));
    for entry in entries.flatten() {
        let path = entry.path();
        let manifest = path.join("plugin.json");
        let entry_file = path.join("main.ts");
        if !manifest.is_file() || !entry_file.is_file() {
            continue;
        }
        let id = path.file_name().unwrap().to_string_lossy().into_owned();
        let source = std::fs::read_to_string(&entry_file).expect("main.ts should be readable");
        out.push((id, source, manifest));
    }
    assert!(
        out.len() >= 3,
        "expected to find the shipped plugins with entry modules; found {}. \
         If plugins moved, this test is looking in the wrong place and is \
         asserting nothing.",
        out.len()
    );
    out
}

/// Every method named in a `call("...")` in `source`, with the byte offset of
/// the call, so a shape check can look at the arguments that follow it.
///
/// Deliberately anchored on `call("` rather than on the method name alone: the
/// prose in these files quotes method names in comments, and a check that
/// matched those would fail on a file explaining the very mistake it guards.
fn calls(source: &str) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    let mut rest = source;
    let mut base = 0usize;
    while let Some(at) = rest.find("call(\"") {
        let start = at + "call(\"".len();
        let Some(end) = rest[start..].find('"') else { break };
        out.push((rest[start..start + end].to_string(), base + start));
        base += start + end;
        rest = &rest[start + end..];
    }
    out
}

fn requested_capabilities(manifest: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(manifest).expect("plugin.json should be readable");
    let parsed: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("{} should be valid JSON: {e}", manifest.display()));
    parsed["capabilities"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
        .unwrap_or_default()
}

/// A method that is not in the closed table is a method that does not exist.
///
/// `required_capability` returning `None` is exactly what the broker treats as
/// unknown, so this asks the same question the host asks, on the same table,
/// without needing a host. It is the check that would have caught
/// `settings.read`, which reads like a method, is spelled like a method, and is
/// a capability.
#[test]
fn every_method_a_shipped_plugin_calls_exists() {
    for (id, source, _) in shipped() {
        for (method, _) in calls(&source) {
            assert!(
                cordial_plugins::protocol::required_capability(&method).is_some(),
                "plugins/{id}/main.ts calls {method:?}, which is not a method. \
                 The closed table is in cordial-plugins/src/protocol.rs. \
                 Note that several capability names look like methods and are \
                 not: settings.read is the capability, settings.get is the call."
            );
        }
    }
}

/// A call the manifest never asked the capability for is always refused.
///
/// The one exception is deliberate and is named here rather than allowed by a
/// pattern, so adding a second one has to be an argument rather than an
/// accident.
#[test]
fn a_shipped_plugin_asks_for_what_its_calls_need() {
    // flag-inspector requests flags.read and not flags.write, then calls
    // flags.set on purpose, so that the boundary is visible in a user's own
    // client output rather than only in a test. Removing this line would make
    // the demonstration fail the suite; removing the demonstration would make
    // the refusal something a plugin author only ever reads about.
    const DELIBERATELY_REFUSED: &[(&str, &str)] = &[("flag-inspector", "flags.set")];

    for (id, source, manifest) in shipped() {
        let held = requested_capabilities(&manifest);
        for (method, _) in calls(&source) {
            if DELIBERATELY_REFUSED.contains(&(id.as_str(), method.as_str())) {
                continue;
            }
            let needed = cordial_plugins::protocol::required_capability(&method)
                .expect("covered by the test above")
                .name();
            assert!(
                held.iter().any(|c| c == needed),
                "plugins/{id}/main.ts calls {method:?}, which needs {needed:?}, \
                 but its plugin.json requests {held:?}. Every such call comes \
                 back denied, and a plugin that never checks the status of a \
                 reply cannot tell that from success."
            );
        }
    }
}

/// `flags.set` takes a `values` object, and nothing else is accepted.
///
/// The handler's first line is `params.get("values")` and its refusal is
/// "flags.set needs a values object". This is a crude check -- it looks at the
/// text following the call -- and it is here anyway, because the shape it
/// guards is the one that silently did nothing for weeks and a crude check that
/// fires is worth more than an elegant one that does not exist.
#[test]
fn a_shipped_plugin_sets_flags_with_a_values_object() {
    for (id, source, _) in shipped() {
        for (method, at) in calls(&source) {
            if method != "flags.set" {
                continue;
            }
            // Char-boundary safe: these files carry em dashes in their
            // comments, and a byte slice that lands mid-character panics with
            // an error about the string rather than about the plugin.
            let mut end = source.len().min(at + 240);
            while end > at && !source.is_char_boundary(end) {
                end -= 1;
            }
            let tail = &source[at..end];
            assert!(
                tail.contains("values"),
                "plugins/{id}/main.ts calls flags.set without a values object. \
                 The handler in cordial-runtime/src/plugin_host.rs refuses \
                 anything else with \"flags.set needs a values object\". \
                 The call reads: {}",
                tail.lines().take(4).collect::<Vec<_>>().join(" ")
            );
        }
    }
}

/// A dispatcher that looks every line up by id discards every push.
///
/// A reply carries `status` and the `id` it answers; a push carries `event` and
/// no id. `pending.get(line.id)` on a push looks up `undefined`, finds nothing,
/// and drops it -- so a plugin that subscribes to events receives them and
/// never notices. These files are what plugin authors copy, so the split has to
/// be in all of them and not only in the one that needs it.
#[test]
fn every_shipped_plugin_tells_a_push_from_a_reply() {
    for (id, source, _) in shipped() {
        assert!(
            source.contains("event"),
            "plugins/{id}/main.ts never mentions the `event` field, so its \
             dispatcher cannot be separating a push from a reply. A plugin \
             with this shape silently discards every event it is sent."
        );
    }
}

/// A manifest that is valid JSON and not a valid `Manifest` loads as nothing.
///
/// `discover` reports it as "not loadable" and moves on, so a shipped plugin
/// with a typo in a preferences declaration presents to the user as a plugin
/// that has simply vanished from Settings. Parsing as JSON is not the check;
/// parsing as the struct is.
#[test]
fn every_shipped_manifest_deserialises_as_a_manifest() {
    for (id, _, manifest) in shipped() {
        let text = std::fs::read_to_string(&manifest).expect("readable");
        let parsed: Result<cordial_plugins::manifest::Manifest, _> = serde_json::from_str(&text);
        let m = parsed.unwrap_or_else(|e| {
            panic!("plugins/{id}/plugin.json is not a Manifest: {e}");
        });
        assert_eq!(m.id, id, "a plugin's id must match its directory name");
        assert!(!m.entry.is_empty(), "plugins/{id} has an entry module and should declare it");
    }
}

/// Every capability a shipped manifest names must parse.
///
/// An unparseable one is not refused loudly -- it is simply not in the granted
/// set, so the plugin runs with less than it asked for and the prompt the user
/// answered did not mention it.
#[test]
fn every_capability_a_shipped_manifest_names_is_real() {
    for (id, _, manifest) in shipped() {
        for name in requested_capabilities(&manifest) {
            assert!(
                cordial_plugins::capability::Capability::parse(&name).is_some(),
                "plugins/{id}/plugin.json asks for {name:?}, which is not a capability"
            );
        }
    }
}

/// A choice field whose default is not one of its own options.
///
/// `Field::default_value` hands the default straight back, so the row opens
/// showing a value the combo cannot select and the plugin is given a string its
/// own list would reject. Cheap to get wrong by editing the options and not the
/// default, which is exactly what happens when a mode is renamed.
#[test]
fn a_declared_choice_defaults_to_one_of_its_own_options() {
    use cordial_plugins::preferences::Field;
    for (id, _, manifest) in shipped() {
        let text = std::fs::read_to_string(&manifest).expect("readable");
        let m: cordial_plugins::manifest::Manifest =
            serde_json::from_str(&text).expect("covered by the test above");
        for field in &m.preferences {
            let Field::Choice { default, options } = &field.field else { continue };
            assert!(
                options.iter().any(|o| &o.value == default),
                "plugins/{id}: preference {:?} defaults to {default:?}, which is not \
                 one of its options {:?}",
                field.key,
                options.iter().map(|o| &o.value).collect::<Vec<_>>()
            );
        }
    }
}

/// The Discord application id the shipped presence plugin falls back to.
///
/// Three things have to agree and nothing else makes them: the constant in
/// `main.ts`, the assertion in `discord_presence_plugin.rs`, and the id Cordial
/// is actually registered under. A wrong digit is invisible in testing --
/// Discord's IPC accepts an id it cannot look up and simply shows the user
/// nothing -- so it is pinned here rather than trusted to review.
#[test]
fn the_presence_plugin_falls_back_to_cordials_real_application_id() {
    let (_, main, _) = shipped()
        .into_iter()
        .find(|(id, _, _)| id == "discord-presence")
        .expect("the shipped discord-presence plugin should be there");
    assert!(
        main.contains(r#"const DEFAULT_CLIENT_ID = "1543200871767212062";"#),
        "plugins/discord-presence/main.ts no longer declares Cordial's registered \
         application id as its fallback"
    );
    assert!(
        !main.contains("1234567890123456"),
        "the placeholder application id is back in plugins/discord-presence/main.ts"
    );
}

/// A plugin that reads a preference must ask for the capability that allows it.
///
/// `preferences.get` sits under `settings.read` rather than having one of its
/// own, and the answers also arrive unasked in the handshake -- which is the
/// trap. A plugin can read `payload.preferences` without ever calling anything,
/// so forgetting the capability does not fail loudly at a call site. It just
/// makes the field permanently null and the user's answer permanently ignored.
#[test]
fn a_plugin_declaring_preferences_also_requests_settings_read() {
    for (id, source, manifest) in shipped() {
        let text = std::fs::read_to_string(&manifest).expect("readable");
        let m: cordial_plugins::manifest::Manifest =
            serde_json::from_str(&text).expect("covered by the test above");
        if m.preferences.is_empty() || !source.contains("preferences") {
            continue;
        }
        assert!(
            m.capabilities.iter().any(|c| c == "settings.read"),
            "plugins/{id}: reads preferences but does not request settings.read, \
             so the answers arrive as null and the user's choice is silently ignored"
        );
    }
}
