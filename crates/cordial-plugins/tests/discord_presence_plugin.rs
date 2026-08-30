//! Runs the real `plugins/discord-presence` plugin end to end: discovered
//! from disk, granted exactly what it requests, spawned as a real Deno
//! process with no permissions, driven by real lifecycle pushes, and its
//! `presence.set`/`presence.clear` calls followed all the way through
//! `Session` and `DiscordPresence` to Discord's actual IPC wire framing —
//! landing on a local Unix-socket test double standing in for Discord.
//!
//! That last part is the honest limit of what this test proves. It shows the
//! whole brokered path is wired correctly and speaks the framing Discord's
//! IPC documents; it does not and cannot show a real Discord client rendering
//! the activity, because no Discord client is running in this environment.
//! See the written report for that caveat stated plainly, per AGENTS.md.

use cordial_plugins::host::{Plugin, Session};
use cordial_plugins::protocol::Response;
use cordial_plugins::{manifest, capability::Capability};
use serde_json::Value;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

fn write_frame(stream: &mut UnixStream, opcode: u32, body: &Value) {
    let bytes = serde_json::to_vec(body).unwrap();
    stream.write_all(&opcode.to_le_bytes()).unwrap();
    stream.write_all(&(bytes.len() as u32).to_le_bytes()).unwrap();
    stream.write_all(&bytes).unwrap();
}

fn read_frame(stream: &mut UnixStream) -> (u32, Value) {
    let mut header = [0u8; 8];
    stream.read_exact(&mut header).unwrap();
    let opcode = u32::from_le_bytes(header[0..4].try_into().unwrap());
    let len = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).unwrap();
    (opcode, serde_json::from_slice(&body).unwrap())
}

/// A local stand-in for Discord's IPC server: accepts one connection,
/// acknowledges the handshake, then acknowledges and forwards every
/// SET_ACTIVITY frame it is sent (both a real activity and the `null`
/// activity `presence.clear` sends) until told to stop.
fn spawn_fake_discord(path: PathBuf, frames_expected: usize) -> std::sync::mpsc::Receiver<(u32, Value)> {
    let listener = UnixListener::bind(&path).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let (op, handshake) = read_frame(&mut stream);
        tx.send((op, handshake)).unwrap();
        write_frame(&mut stream, 1, &serde_json::json!({"evt": "READY"}));

        for _ in 0..frames_expected {
            let (op, frame) = read_frame(&mut stream);
            tx.send((op, frame)).unwrap();
            write_frame(&mut stream, 1, &serde_json::json!({"evt": null}));
        }
    });
    rx
}

#[test]
fn discord_presence_follows_lifecycle_events_all_the_way_to_the_wire() {
    if std::process::Command::new("deno").arg("--version").output().is_err() {
        eprintln!("skipping: deno is not installed");
        return;
    }

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
    let found = manifest::discover(&root);
    let plugin = found
        .iter()
        .find(|p| p.manifest.id == "discord-presence")
        .expect("the shipped discord-presence plugin should be discoverable");
    assert!(plugin.requested.contains(&Capability::LifecycleRead));
    assert!(plugin.requested.contains(&Capability::PresenceSet));

    let dir = std::env::temp_dir().join(format!("cordial-discord-presence-plugin-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("XDG_RUNTIME_DIR", &dir);
    // Two frames expected: the "launch" presence.set and the "shutdown"
    // presence.clear (which sends activity: null through the same op).
    let rx = spawn_fake_discord(dir.join("discord-ipc-0"), 2);

    let mut session = Session::new();
    session.broker.grant("discord-presence", plugin.requested.iter().copied());

    let entry = plugin.entry_path().unwrap();
    let proc = Plugin::spawn("discord-presence", &entry).expect("deno should start");
    session.add_plugin(proc);

    let mut logs: Vec<String> = Vec::new();
    let mut sent_launch = false;
    let mut sent_shutdown = false;
    while let Some(req) = session.plugin_mut("discord-presence").unwrap().next_request() {
        let Ok(req) = req else { break };

        if req.method == "log.write" {
            let message = req.params["message"].as_str().unwrap_or_default().to_string();
            session
                .plugin_mut("discord-presence")
                .unwrap()
                .reply(&Response::Ok { id: req.id, result: Value::Null })
                .unwrap();

            if message.contains("lifecycle.subscribe came back: ok") && !sent_launch {
                sent_launch = true;
                session.push_lifecycle("launch");
            }
            if message.contains("presence.set on cordial/client.launch came back: ok") && !sent_shutdown {
                sent_shutdown = true;
                session.push_lifecycle("shutdown");
                // Delivery is asynchronous (ADR-026), so the shutdown event
                // has to be flushed or it loses the race against teardown --
                // which is exactly the bug this line exists because of.
                session.flush_events(std::time::Duration::from_secs(2));
            }
            logs.push(message);
            if logs.iter().any(|l| l.contains("presence.clear on shutdown came back")) {
                break;
            }
            continue;
        }

        let res = session.handle("discord-presence", &req);
        session.plugin_mut("discord-presence").unwrap().reply(&res).unwrap();
    }
    session.plugin_mut("discord-presence").unwrap().kill();

    let joined = logs.join("\n");
    assert!(joined.contains("lifecycle.subscribe came back: ok"), "got:\n{joined}");
    assert!(joined.contains("presence.set on cordial/client.launch came back: ok"), "got:\n{joined}");
    assert!(joined.contains("presence.clear on shutdown came back: ok"), "got:\n{joined}");

    // And the wire: the fake Discord actually received a well-formed
    // handshake and both frames, in the shape DiscordPresence promises.
    let (op, handshake) = rx.recv().unwrap();
    assert_eq!(op, 0);
    assert_eq!(handshake["v"], 1);
    // Pinned to the real registered application rather than left loose. The
    // plugin takes this from its `client_id` preference when the user has set
    // one, and this session grants `settings.read` with no store behind it, so
    // `preferences` arrives null and the shipped default is what reaches the
    // wire. That is the path almost every user is on, and a typo in the
    // constant would otherwise only show up as Discord quietly displaying
    // nothing.
    assert_eq!(handshake["client_id"], "1543200871767212062");

    let (op, set_activity) = rx.recv().unwrap();
    assert_eq!(op, 1);
    assert_eq!(set_activity["cmd"], "SET_ACTIVITY");
    assert_eq!(set_activity["args"]["activity"]["details"], "Using Cordial");
    assert_eq!(set_activity["args"]["activity"]["state"], "Starting up");

    let (op, clear_activity) = rx.recv().unwrap();
    assert_eq!(op, 1);
    assert_eq!(clear_activity["cmd"], "SET_ACTIVITY");
    assert!(clear_activity["args"]["activity"].is_null(), "clearing must send a null activity: {clear_activity}");

    let _ = std::fs::remove_dir_all(&dir);
}
