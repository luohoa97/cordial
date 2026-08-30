//! The three properties that stop gamepad support being a stub that lies.
//!
//! All of this was checked once, by hand, during the merge in `3ed78ee`, with a
//! test that was deliberately thrown away. That is the wrong place for it: the
//! gate is the only thing standing between a partial symbol resolution and a
//! client that reports a connected pad, accepts button events, and never tells
//! the engine which buttons exist -- which looks like working gamepad support
//! while half the inputs silently do nothing. AGENTS.md calls that the worst
//! shape a stub can take, because the failure surfaces nowhere near its cause.
//!
//! `crates/cordial-runtime/src/android/gamepad.rs` has five tests and every one
//! of them is about decoding a joydev packet or a mapping table. None touches
//! the gate, the on-unless-switched-off default, or the type argument.
//!
//! **One test for the gate, and it is one on purpose.** The six pointers live in
//! process-wide `AtomicPtr` statics, so every test in this binary shares them,
//! and `set_gamepad_natives` deliberately does not clear a previous store when
//! it refuses a partial set. The partial case therefore has to run before the
//! complete one or it is testing nothing. A second `#[test]` here would order
//! itself however the harness felt like, which is the same reasoning
//! `profile_configuration.rs` records for its own single test.
//!
//! The environment cases need child processes rather than more tests. Both
//! `enabled()` and `gamepad_type()` cache into a `OnceLock`, so a process gets
//! one answer for its whole life and cannot be asked a second question.

use std::process::Command;

/// Four fake pointers. Never dereferenced: the gate only ever asks whether a
/// pointer is null, and a real one would need an engine to resolve it against.
fn fake(n: usize) -> *mut std::ffi::c_void {
    n as *mut std::ffi::c_void
}

/// A partial symbol set must be refused whole, and must not half-arm the path.
///
/// The failure this exists to catch: `libroblox.so` renames or drops one of the
/// six between builds, five resolve, and Cordial wires up axis and button
/// delivery without ever calling
/// `nativeSetGamepadSupportedKeyWithGamepadType`. The engine is then told a pad
/// arrived and never told what it can do, so some inputs work and some vanish.
/// A user reports "my controller half works" and nothing in the log says why.
///
/// The order below is load-bearing, not stylistic -- see the module comment.
#[test]
fn a_missing_native_refuses_the_whole_set_and_arms_nothing() {
    use cordial_runtime::android::input::{gamepad_natives_ready, set_gamepad_natives};

    assert!(
        !gamepad_natives_ready(),
        "the statics must start null, or the partial case below is measuring a previous store"
    );

    // Five resolve, the capability-declaration native does not.
    let missing = set_gamepad_natives(fake(1), fake(2), fake(3), fake(4), std::ptr::null_mut(), fake(6));
    assert_eq!(
        missing,
        vec!["nativeSetGamepadSupportedKeyWithGamepadType"],
        "the refusal must name what was missing; a bare bool would leave a build \
         with a renamed symbol undiagnosable from a log"
    );
    assert!(
        !gamepad_natives_ready(),
        "a refused set must store nothing -- storing five of six is the half-armed \
         state this gate exists to make unreachable"
    );

    // Two missing, to show the report is not a special case for one name.
    let missing = set_gamepad_natives(
        std::ptr::null_mut(),
        fake(2),
        fake(3),
        fake(4),
        fake(5),
        std::ptr::null_mut(),
    );
    assert_eq!(
        missing,
        vec![
            "nativeGamepadConnectEventWithGamepadType",
            "nativeSetGamepadSupportedMotionWithGamepadType"
        ]
    );
    assert!(!gamepad_natives_ready());

    // All six, which is the only state that may arm it.
    let missing = set_gamepad_natives(fake(1), fake(2), fake(3), fake(4), fake(5), fake(6));
    assert!(missing.is_empty(), "a complete set must report nothing missing");
    assert!(gamepad_natives_ready(), "a complete set must arm the path");
}

/// Re-run this test binary as a child, with `vars` set, and hand back its stderr.
///
/// A child rather than a thread because `enabled()` and `gamepad_type()` are
/// `OnceLock`s: the first read in a process fixes the answer for that process.
/// Asking twice in one process asks the same question twice.
fn poll_in_child(vars: &[(&str, &str)]) -> String {
    let exe = std::env::current_exe().expect("test binary path");
    let mut cmd = Command::new(exe);
    // Run only the hook, and let its output through: libtest swallows stderr
    // unless told not to, and the announce this reads is an `eprintln!`.
    cmd.args(["--exact", "poll_child_hook", "--nocapture", "--test-threads", "1"]);
    cmd.env("CORDIAL_GAMEPAD_POLL_CHILD", "1");
    // Cleared explicitly rather than assumed absent: this binary inherits the
    // developer's environment, and a `CORDIAL_GAMEPAD` they set for their own
    // run would otherwise decide both arms below. It mattered in both
    // directions: under the old off-by-default contract an inherited `=1` made
    // the off case a false pass, and now an inherited `=0` would make the
    // on case a false failure.
    cmd.env_remove("CORDIAL_GAMEPAD");
    cmd.env_remove("CORDIAL_GAMEPAD_PROBE");
    cmd.env_remove("CORDIAL_GAMEPAD_TYPE");
    for (k, v) in vars {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("re-run self");
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Gamepad support runs unless it is switched off, and the switch must work.
///
/// **This asserted the opposite until 0.13.0, and the reason it changed is
/// worth keeping rather than deleting.** The old contract was off-by-default,
/// on the argument that announcing a pad with an unverified `gamepadType`
/// would "draw controller glyphs at users who have no controller, from an
/// ordinal that is still a guess". The ordinal is still a guess. What that
/// argument left out is the cost: no controller support of any kind, on a
/// runtime whose users include handhelds. Sober #584 and #1810 are the same
/// wrong-ordinal symptom on the neighbouring runtime and neither reports a
/// button that stopped working -- the glyphs are wrong and the pad works.
///
/// So the failure this now exists to catch is the mirror image: someone makes
/// the host poll unconditional and `CORDIAL_GAMEPAD=0` stops turning it off,
/// leaving a user who is hitting a real problem -- a pad that steals focus, a
/// joydev node that misbehaves -- with no way out and a documented switch that
/// lies.
///
/// The control is the same child with the same probe and the switch set to 0.
/// Without it this asserts only that leaving a variable unset does something,
/// not that setting it stops it, which is the half a user depends on.
#[test]
fn the_host_poll_runs_unless_switched_off() {
    const ANNOUNCE: &str = "announcing a synthetic pad";

    let on = poll_in_child(&[("CORDIAL_GAMEPAD_PROBE", "1")]);
    assert!(
        on.contains(ANNOUNCE),
        "the probe did not run with CORDIAL_GAMEPAD unset; on-by-default is not \
         holding.\nstderr: {on}"
    );

    let off = poll_in_child(&[("CORDIAL_GAMEPAD", "0"), ("CORDIAL_GAMEPAD_PROBE", "1")]);
    assert!(
        !off.contains(ANNOUNCE),
        "the probe still announced with CORDIAL_GAMEPAD=0, so the off switch does \
         not work and the arm above proves nothing.\nstderr: {off}"
    );
}

/// The type argument must reach the announce, because it is the whole experiment.
///
/// `gamepadType`'s ordinals are unestablished -- no capture in `docs/traces/`
/// has a pad in it, and this build ships no type-less connect native. The way
/// out is to sweep `CORDIAL_GAMEPAD_TYPE=N` and photograph which N makes the
/// engine draw PlayStation glyphs. If the variable stops being read, that sweep
/// silently measures one value N times and the ordinals stay unknown while
/// looking investigated.
///
/// It first appeared as a *failing* assertion during the merge, from a value
/// inherited out of a sibling test's environment -- which is itself the evidence
/// the variable is read, and why `poll_in_child` clears it.
#[test]
fn the_type_argument_reaches_the_announce() {
    let out = poll_in_child(&[
        ("CORDIAL_GAMEPAD", "1"),
        ("CORDIAL_GAMEPAD_PROBE", "1"),
        ("CORDIAL_GAMEPAD_TYPE", "7"),
    ]);
    assert!(
        out.contains("gamepadType=7"),
        "CORDIAL_GAMEPAD_TYPE=7 did not reach the announce, so the ordinal sweep \
         would compare a value against itself.\nstderr: {out}"
    );
}

/// The child half of [`poll_in_child`], and a no-op in an ordinary run.
///
/// A test rather than a `main` hook because an integration test has no `main` of
/// its own to hook -- libtest supplies it. The parent re-runs this binary asking
/// for this test by name; when the variable is absent, which is every normal
/// `cargo test`, it returns immediately and asserts nothing.
///
/// It cannot be `#[ignore]`d into the child-only role: `--exact` still skips an
/// ignored test unless `--include-ignored` is passed, and hiding the guard in a
/// second flag makes the mechanism harder to read for no gain.
#[test]
fn poll_child_hook() {
    if std::env::var_os("CORDIAL_GAMEPAD_POLL_CHILD").is_none() {
        return;
    }
    // `poll()` returns before the probe unless the gate is armed -- which is the
    // property the first test in this file exists to protect, so arming it here
    // is deliberate rather than a workaround.
    //
    // The six pointers are fake, and calling through them is safe for a reason
    // that is not obvious: every one of the C++ shims asks
    // `cordial::process_env()` for a JavaVM first and returns -1 when there is
    // none, before it casts the pointer to a function and calls it. A test
    // process has no JavaVM, so the announce reaches the shim, the shim
    // declines, and nothing is ever dereferenced. If that ordering is ever
    // changed so the cast happens first, this test turns into a wild call
    // through the address 1 and the crash will point straight at the change.
    cordial_runtime::android::input::set_gamepad_natives(
        fake(1), fake(2), fake(3), fake(4), fake(5), fake(6),
    );
    cordial_runtime::android::gamepad::poll();
}
