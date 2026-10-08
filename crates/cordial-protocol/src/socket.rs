//! Where the socket lives, and how both sides reach it when the path is too
//! long (spec sections 2 and 9).
//!
//! The launcher creates `<profile>/runtime/<session>/` with mode `0700`; the
//! runtime listens on `ctl.sock` inside it and the launcher connects. A
//! `sockaddr_un` holds 108 bytes including the NUL, and a profile directory
//! under a Flatpak or beside a long `XDG_DATA_HOME` can use most of that before
//! the session name is added, so a path that does not fit is reached through
//! `/proc/self/fd/<dirfd>/ctl.sock` instead: open the directory, and name the
//! socket relative to the descriptor. Both [`bind`] and [`connect`] do this
//! themselves, because a listener that fell back and a launcher that did not
//! would each be fine alone and unable to find one another.
//!
//! Only `std`, and nothing here is `unsafe`.

use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

/// The socket's file name inside a session directory.
pub const SOCKET_NAME: &str = "ctl.sock";

/// `sizeof(sockaddr_un.sun_path)` on Linux, NUL included.
pub const SUN_PATH_LEN: usize = 108;

/// Characters in a session name. Eight keeps the whole path short enough that
/// the descriptor fallback is the exception and not the rule.
pub const SESSION_LEN: usize = 8;

/// The socket inside `session_dir`.
pub fn socket_path(session_dir: &Path) -> PathBuf {
    session_dir.join(SOCKET_NAME)
}

/// Whether a socket at `path` can be named directly.
pub fn fits(path: &Path) -> bool {
    path.as_os_str().len() < SUN_PATH_LEN
}

/// A fresh session name: eight lowercase hex digits from the kernel's random
/// source, falling back to the clock and process id if `/dev/urandom` cannot be
/// read. Unguessable is not the property wanted, the directory is `0700` for
/// that; distinct across the launches one profile will see is.
pub fn session_id() -> String {
    use std::io::Read;
    let mut raw = [0u8; 4];
    let read = File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut raw));
    if read.is_err() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        raw = (nanos ^ std::process::id().rotate_left(16)).to_le_bytes();
    }
    raw.iter().map(|b| format!("{b:02x}")).collect()
}

/// Whether `name` is a session name this crate would have made: [`SESSION_LEN`]
/// lowercase hex digits. A launcher uses it to tell a session directory from
/// anything else a user left under `runtime/`.
pub fn is_session_id(name: &str) -> bool {
    name.len() == SESSION_LEN && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Create `dir` (and its parents) with mode `0700`, and narrow it if it already
/// existed wider. The permission lives on the directory because a socket takes
/// the process umask at `bind`, and a `chmod` after it leaves a window; making
/// the path unreachable to anyone else first closes it.
pub fn prepare_dir(dir: &Path) -> io::Result<()> {
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// Listen on `ctl.sock` in `session_dir`, replacing a stale socket file.
///
/// The caller owns the directory: a socket left by a runtime that was killed is
/// removed here, which is only safe because the profile lock has already
/// established that no other runtime owns it.
pub fn bind(session_dir: &Path) -> io::Result<UnixListener> {
    let path = socket_path(session_dir);
    let _ = std::fs::remove_file(&path);
    if fits(&path) {
        return UnixListener::bind(&path);
    }
    let dir = File::open(session_dir)?;
    UnixListener::bind(through(&dir))
}

/// Connect to the runtime listening in `session_dir`.
pub fn connect(session_dir: &Path) -> io::Result<UnixStream> {
    let path = socket_path(session_dir);
    if fits(&path) {
        return UnixStream::connect(&path);
    }
    let dir = File::open(session_dir)?;
    UnixStream::connect(through(&dir))
}

/// `/proc/self/fd/<fd>/ctl.sock`: the socket named relative to a directory
/// descriptor, which is short whatever the real path is. `dir` must outlive the
/// call it is used in.
fn through(dir: &File) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", dir.as_raw_fd())).join(SOCKET_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    struct Scratch(PathBuf);
    impl Scratch {
        fn new(tag: &str, depth: usize) -> Self {
            let mut dir = std::env::temp_dir().join(format!("cordial-proto-socket-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            // A directory name of 40 characters, repeated, to push the socket
            // past what `sun_path` holds.
            for _ in 0..depth {
                dir = dir.join("d".repeat(40));
            }
            prepare_dir(&dir).unwrap();
            Scratch(dir)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let root = self.0.ancestors().find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("cordial-proto-socket-")));
            if let Some(root) = root {
                let _ = std::fs::remove_dir_all(root);
            }
        }
    }

    fn round_trip(dir: &Path) {
        let listener = bind(dir).unwrap();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4];
            s.read_exact(&mut buf).unwrap();
            s.write_all(&buf).unwrap();
        });
        let mut c = connect(dir).unwrap();
        c.write_all(b"ping").unwrap();
        let mut back = [0u8; 4];
        c.read_exact(&mut back).unwrap();
        assert_eq!(&back, b"ping");
        server.join().unwrap();
    }

    #[test]
    fn a_short_path_is_bound_and_reached_directly() {
        let d = Scratch::new("short", 0);
        assert!(fits(&socket_path(&d.0)));
        round_trip(&d.0);
    }

    #[test]
    fn a_path_over_the_sun_path_limit_is_reached_through_the_directory_descriptor() {
        let d = Scratch::new("long", 3);
        let path = socket_path(&d.0);
        assert!(!fits(&path), "{} bytes is meant to be too long", path.as_os_str().len());
        // The control: the direct bind of that path fails, so the fallback is
        // what made the round trip work and not a path that fit after all.
        assert!(UnixListener::bind(&path).is_err());
        round_trip(&d.0);
    }

    #[test]
    fn a_stale_socket_is_replaced() {
        let d = Scratch::new("stale", 0);
        drop(bind(&d.0).unwrap());
        assert!(socket_path(&d.0).exists(), "a dropped listener leaves its file behind");
        round_trip(&d.0);
    }

    #[test]
    fn prepare_dir_narrows_a_directory_that_was_made_wide() {
        let d = Scratch::new("mode", 0);
        std::fs::set_permissions(&d.0, std::fs::Permissions::from_mode(0o755)).unwrap();
        prepare_dir(&d.0).unwrap();
        assert_eq!(std::fs::metadata(&d.0).unwrap().permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn session_ids_are_eight_hex_digits_and_differ() {
        let (a, b) = (session_id(), session_id());
        assert!(is_session_id(&a) && is_session_id(&b));
        assert_ne!(a, b);
        assert!(!is_session_id("ABCDEF01") && !is_session_id("abc") && !is_session_id("abcdefg1"));
    }
}
