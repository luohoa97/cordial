//! What the launcher remembers about this session, for the log export.
//!
//! **The launcher never kept a log.** Everything it says goes to its own stdout
//! and stderr, which on a desktop launch is the journal and under Flatpak is
//! nowhere a user knows to look, and the client's output survives only as the
//! last 200 lines on the crash page. A bug report that needs "what did it
//! print" therefore asked the reporter to relaunch from a terminal and paste
//! what scrolled past -- which is the step that issue threads here keep
//! stalling on.
//!
//! So the launcher now keeps three small rings in memory, and the Report a
//! Problem screen can write them out (see [`crate::log_export`]):
//!
//! - everything the launcher itself printed, by teeing its stdout and stderr
//!   (the client's output is echoed to the launcher's stdout, so this holds
//!   that too, further back than the crash page's 200 lines);
//! - the most recent client run's output tail, the very buffer the crash page
//!   reads;
//! - the last few `[cordial] health:` lines, which the client prints every
//!   thirty seconds and which scroll out of both of the above in a long game.
//!
//! Nothing here is written to disk. Raw lines are held; redaction happens at
//! export, over every source together, so a user id first seen in one file is
//! the same placeholder in all of them.
//!
//! ## The tee, and what it changes
//!
//! Capturing `eprintln!` means standing between the process and its stderr,
//! because there is no logging layer in the shell to hook. [`install`] points
//! fd 1 and fd 2 at pipes and forwards every byte to the original descriptors
//! from a thread. The cost is that the two streams are no longer terminals as
//! far as `isatty` can tell, so GLib stops colouring its warnings. Everything
//! still arrives where it did. `CORDIAL_NO_LOG_CAPTURE=1` skips the whole
//! thing, which is the control for "did the tee change this".

use std::collections::VecDeque;
use std::io::Read;
use std::sync::{Arc, Mutex, OnceLock};

/// Lines of the launcher's own output kept. Roughly a long session's
/// narration; the client's health line alone is two a minute.
const LAUNCHER_LINES: usize = 3000;
/// Health lines kept, newest last.
const HEALTH_LINES: usize = 40;
/// A line longer than this is cut. A runaway line must not be what fills the
/// memory of the process whose job is to still be alive to report it.
const MAX_LINE: usize = 4096;

/// The shared type of the client-output tail `launch.rs` keeps per instance.
pub type Tail = Arc<Mutex<VecDeque<String>>>;

struct State {
    launcher: Mutex<VecDeque<String>>,
    health: Mutex<VecDeque<String>>,
    last_client: Mutex<Option<(Tail, String)>>,
}

fn state() -> &'static State {
    static STATE: OnceLock<State> = OnceLock::new();
    STATE.get_or_init(|| State {
        launcher: Mutex::new(VecDeque::new()),
        health: Mutex::new(VecDeque::new()),
        last_client: Mutex::new(None),
    })
}

fn push(ring: &Mutex<VecDeque<String>>, cap: usize, line: &str) {
    let mut line = line.to_string();
    if line.len() > MAX_LINE {
        let mut cut = MAX_LINE;
        while !line.is_char_boundary(cut) {
            cut -= 1;
        }
        line.truncate(cut);
        line.push_str(" ...(line cut)");
    }
    let mut ring = ring.lock().unwrap_or_else(|e| e.into_inner());
    if ring.len() == cap {
        ring.pop_front();
    }
    ring.push_back(line);
}

/// Record one line the launcher printed.
pub fn launcher_line(line: &str) {
    push(&state().launcher, LAUNCHER_LINES, line);
}

/// Record one `[cordial] health:` line from a client, if it is one.
pub fn health_line(line: &str) {
    if line.contains("[cordial] health:") {
        push(&state().health, HEALTH_LINES, line);
    }
}

/// Note that `tail` is now the newest client's output, and what it was started
/// with. Older clients keep writing into their own buffers, which are no longer
/// reachable from here: the export is about the most recent run.
pub fn set_last_client(tail: Tail, command_line: &str) {
    *state().last_client.lock().unwrap_or_else(|e| e.into_inner()) = Some((tail, command_line.to_string()));
}

/// Everything the launcher printed this session, oldest first.
pub fn launcher_lines() -> Vec<String> {
    state().launcher.lock().unwrap_or_else(|e| e.into_inner()).iter().cloned().collect()
}

/// The last health lines, oldest first.
pub fn health_lines() -> Vec<String> {
    state().health.lock().unwrap_or_else(|e| e.into_inner()).iter().cloned().collect()
}

/// The newest client's output tail and the command it was started with, or
/// `None` if no client has been started this session.
pub fn last_client() -> Option<(Vec<String>, String)> {
    let guard = state().last_client.lock().unwrap_or_else(|e| e.into_inner());
    let (tail, command) = guard.as_ref()?;
    let lines = tail.lock().unwrap_or_else(|e| e.into_inner()).iter().cloned().collect();
    Some((lines, command.clone()))
}

/// Start keeping the launcher's own output. Call once, early, in the process
/// that shows the window. Idempotent, and a failure to set up leaves the
/// process exactly as it was.
pub fn install() {
    static ONCE: OnceLock<()> = OnceLock::new();
    if std::env::var_os("CORDIAL_NO_LOG_CAPTURE").is_some() {
        return;
    }
    ONCE.get_or_init(|| {
        for fd in [libc::STDOUT_FILENO, libc::STDERR_FILENO] {
            if let Err(e) = tee(fd) {
                // Nothing has changed for this descriptor if it failed before
                // the dup2, which is the only step that can; say so on the
                // stream that still works.
                eprintln!("[cordial] not keeping a copy of fd {fd} for the log export: {e}");
            }
        }
    });
}

/// Replace `fd` with a pipe whose reader forwards to the old descriptor and
/// records each line.
fn tee(fd: i32) -> std::io::Result<()> {
    // SAFETY: plain descriptor plumbing. Every fd opened here is either closed
    // below or owned by the reader thread for the life of the process.
    unsafe {
        let saved = libc::dup(fd);
        if saved < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut ends = [0i32; 2];
        if libc::pipe2(ends.as_mut_ptr(), libc::O_CLOEXEC) != 0 {
            let e = std::io::Error::last_os_error();
            libc::close(saved);
            return Err(e);
        }
        if libc::dup2(ends[1], fd) < 0 {
            let e = std::io::Error::last_os_error();
            libc::close(saved);
            libc::close(ends[0]);
            libc::close(ends[1]);
            return Err(e);
        }
        libc::close(ends[1]);
        let reader = ends[0];
        std::thread::Builder::new().name("log-tee".into()).spawn(move || forward(reader, saved)).map(|_| ())
    }
}

/// Read `reader` to EOF, writing each chunk to `out` and recording complete
/// lines. **Keeps draining if `out` has gone**: a reader that stopped on a
/// write error would fill the pipe and block the launcher's next `println!`,
/// which is a much worse failure than losing the terminal.
fn forward(reader: i32, out: i32) {
    use std::os::fd::FromRawFd;
    // SAFETY: both descriptors were created by `tee` and are owned here.
    let mut input = unsafe { std::fs::File::from_raw_fd(reader) };
    let mut output = unsafe { std::fs::File::from_raw_fd(out) };
    let mut buf = [0u8; 8192];
    let mut pending: Vec<u8> = Vec::new();
    let mut out_ok = true;
    loop {
        let n = match input.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        if out_ok {
            use std::io::Write;
            if output.write_all(&buf[..n]).is_err() {
                out_ok = false;
            }
        }
        pending.extend_from_slice(&buf[..n]);
        while let Some(at) = pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = pending.drain(..=at).collect();
            let text = String::from_utf8_lossy(&line[..line.len() - 1]);
            launcher_line(text.trim_end_matches('\r'));
        }
        if pending.len() > MAX_LINE * 4 {
            launcher_line(&String::from_utf8_lossy(&pending));
            pending.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ring_drops_its_oldest_line_and_cuts_a_runaway_one() {
        let ring = Mutex::new(VecDeque::new());
        for i in 0..5 {
            push(&ring, 3, &format!("line {i}"));
        }
        let kept: Vec<_> = ring.lock().unwrap().iter().cloned().collect();
        assert_eq!(kept, ["line 2", "line 3", "line 4"]);

        push(&ring, 3, &"x".repeat(MAX_LINE * 2));
        let last = ring.lock().unwrap().back().cloned().unwrap();
        assert!(last.len() < MAX_LINE + 32 && last.ends_with("(line cut)"), "{}", last.len());
    }

    #[test]
    fn only_health_lines_reach_the_health_ring() {
        let before = health_lines().len();
        health_line("12:00:00.000 [cordial] health: 1061 presents in 31s (34.2/s), 1061 total");
        health_line("12:00:01.000 [cordial] app ready: Startup");
        assert_eq!(health_lines().len(), before + 1);
    }

    #[test]
    fn the_newest_client_is_the_one_reported() {
        let first: Tail = Arc::new(Mutex::new(VecDeque::from(["old".to_string()])));
        let second: Tail = Arc::new(Mutex::new(VecDeque::from(["new".to_string()])));
        set_last_client(first, "run one");
        set_last_client(second, "run two");
        let (lines, command) = last_client().unwrap();
        assert_eq!((lines, command.as_str()), (vec!["new".to_string()], "run two"));
    }
}
