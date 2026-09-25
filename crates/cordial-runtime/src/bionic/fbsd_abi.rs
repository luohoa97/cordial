//! The engine's Linux ABI values, translated for a FreeBSD host.
//!
//! `libroblox.so` passes Linux flag words, option numbers, address families,
//! sockaddr layouts and reads Linux errno numbers; FreeBSD's libc reads every
//! one of those as something else. The translation is `native/freebsd_abi.c`;
//! this registers its table and, more usefully, holds the tests that call each
//! wrapper natively beside the untranslated host call as a control.

use std::ffi::{c_char, c_void, CStr};

/// `native/freebsd_abi.c`'s symbol table.
pub fn overrides() -> Vec<(&'static str, *mut c_void)> {
    #[repr(C)]
    struct Symbol {
        name: *const c_char,
        addr: *mut c_void,
    }
    extern "C" {
        fn cordial_fbsd_abi_symbols(count: *mut usize) -> *const Symbol;
    }
    let mut count = 0usize;
    // SAFETY: the table is a static in freebsd_abi.c and outlives the process.
    let table = unsafe { cordial_fbsd_abi_symbols(&mut count) };
    if table.is_null() {
        return Vec::new();
    }
    // SAFETY: `table` points at `count` initialised entries with static names.
    let entries = unsafe { std::slice::from_raw_parts(table, count) };
    entries
        .iter()
        .map(|e| {
            // SAFETY: each `name` is a string literal in freebsd_abi.c.
            let name = unsafe { CStr::from_ptr(e.name) }.to_str().unwrap_or("");
            (name, e.addr)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::overrides;
    use std::collections::BTreeMap;
    use std::ffi::{c_int, c_void, CString};
    use std::os::unix::fs::PermissionsExt;

    // Linux values, as the engine passes them (bionic kernel uapi headers).
    const LX_O_WRONLY: c_int = 0x1;
    const LX_O_CREAT: c_int = 0x40;
    const LX_O_TRUNC: c_int = 0x200;
    const LX_O_NONBLOCK: c_int = 0x800;
    const LX_O_CLOEXEC: c_int = 0x80000;
    const LX_AF_INET: c_int = 2;
    const LX_AF_INET6: c_int = 10;
    const LX_SOCK_STREAM: c_int = 1;
    const LX_SOL_SOCKET: c_int = 1;
    const LX_SO_ERROR: c_int = 4;
    const LX_F_GETFL: c_int = 3;
    const LX_F_SETFL: c_int = 4;
    const LX_EAGAIN: c_int = 11;
    const LX_EINPROGRESS: c_int = 115;
    const LX_ECONNREFUSED: c_int = 111;
    const LX_POLLOUT: i16 = 0x4;

    // FreeBSD values, for reading the result back through the host.
    const FB_O_NONBLOCK: c_int = 0x4;
    const FB_F_GETFD: c_int = 1;
    const FB_F_GETFL: c_int = 3;
    const FB_F_SETFL: c_int = 4;
    const FB_FD_CLOEXEC: c_int = 1;
    const FB_AF_INET: u8 = 2;

    #[repr(C)]
    struct PollFd {
        fd: c_int,
        events: i16,
        revents: i16,
    }

    // `bionic/mod.rs` declares `pipe` as a bare `fn()` only to take its
    // address for the symbol table; here it is called, so it needs its type.
    #[allow(clashing_extern_declarations)]
    extern "C" {
        fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
        fn pipe(fds: *mut c_int) -> c_int;
        fn close(fd: c_int) -> c_int;
        fn socket(domain: c_int, ty: c_int, proto: c_int) -> c_int;
        fn eventfd(init: u32, flags: c_int) -> c_int;
        fn bind(fd: c_int, addr: *const c_void, len: u32) -> c_int;
        fn listen(fd: c_int, backlog: c_int) -> c_int;
        fn getsockname(fd: c_int, addr: *mut c_void, len: *mut u32) -> c_int;
        fn connect(fd: c_int, addr: *const c_void, len: u32) -> c_int;
        fn __error() -> *mut c_int;
    }

    fn sym(name: &str) -> *mut c_void {
        overrides()
            .into_iter()
            .find(|(n, _)| *n == name)
            .unwrap_or_else(|| panic!("{name} is not in freebsd_abi.c's table"))
            .1
    }

    macro_rules! wrapper {
        ($name:literal, $ty:ty) => {{
            let p = sym($name);
            // SAFETY: the table entry is the C function of this signature.
            unsafe { std::mem::transmute::<*mut c_void, $ty>(p) }
        }};
    }

    /// The errno the engine would read, through the registered `__errno`.
    fn engine_errno() -> c_int {
        let f = wrapper!("__errno", unsafe extern "C" fn() -> *mut c_int);
        // SAFETY: returns this thread's errno slot.
        unsafe { *f() }
    }

    /// The raw slot, as a caller that hoisted the `__attribute_const__`
    /// `__errno()` pointer above the call would read it.
    fn raw_errno() -> c_int {
        // SAFETY: FreeBSD's per-thread errno slot.
        unsafe { *__error() }
    }

    fn host_fl(fd: c_int) -> c_int {
        // SAFETY: plain fcntl on an fd this test owns.
        unsafe { fcntl(fd, FB_F_GETFL) }
    }

    fn host_fd_flags(fd: c_int) -> c_int {
        // SAFETY: plain fcntl on an fd this test owns.
        unsafe { fcntl(fd, FB_F_GETFD) }
    }

    /// Linux-layout sockaddr_in: u16 family, then port and address.
    fn lx_sockaddr_in(port_be: u16, addr: [u8; 4]) -> [u8; 16] {
        let mut sa = [0u8; 16];
        sa[0..2].copy_from_slice(&(LX_AF_INET as u16).to_ne_bytes());
        sa[2..4].copy_from_slice(&port_be.to_ne_bytes());
        sa[4..8].copy_from_slice(&addr);
        sa
    }

    /// A FreeBSD listening socket on 127.0.0.1, and its port (network order).
    fn host_listener(do_listen: bool) -> (c_int, u16) {
        unsafe {
            let s = socket(2, 1, 0);
            assert!(s >= 0);
            let mut sa = [0u8; 16];
            sa[0] = 16;
            sa[1] = FB_AF_INET;
            sa[4..8].copy_from_slice(&[127, 0, 0, 1]);
            assert_eq!(bind(s, sa.as_ptr().cast(), 16), 0);
            if do_listen {
                assert_eq!(listen(s, 4), 0);
            }
            let mut out = [0u8; 16];
            let mut len = 16u32;
            assert_eq!(getsockname(s, out.as_mut_ptr().cast(), &mut len), 0);
            (s, u16::from_ne_bytes([out[2], out[3]]))
        }
    }

    #[test]
    fn every_translated_name_wins_in_the_final_symbol_map() {
        // symtab.rs collects function_overrides() into a map where a later
        // duplicate replaces an earlier one. Every name this file translates
        // must end up pointing here, or the engine gets the untranslated one.
        let map: BTreeMap<&str, *mut c_void> =
            crate::bionic::function_overrides().into_iter().collect();
        for (name, addr) in overrides() {
            assert_eq!(map.get(name).copied(), Some(addr), "{name} is shadowed");
        }
    }

    #[test]
    fn eventfd_with_linux_flags_is_nonblocking_and_cloexec() {
        let lx_eventfd = wrapper!("eventfd", unsafe extern "C" fn(u32, c_int) -> c_int);
        // Control: the host reads Linux 0x800|0x80000 as garbage and refuses.
        let raw = unsafe { eventfd(0, LX_O_NONBLOCK | LX_O_CLOEXEC) };
        assert_eq!(raw, -1, "host eventfd accepted Linux flags; control is void");

        let fd = unsafe { lx_eventfd(0, LX_O_NONBLOCK | LX_O_CLOEXEC) };
        assert!(fd >= 0, "eventfd failed, engine errno {}", engine_errno());
        assert_ne!(host_fl(fd) & FB_O_NONBLOCK, 0, "not non-blocking");
        assert_ne!(host_fd_flags(fd) & FB_FD_CLOEXEC, 0, "not close-on-exec");
        unsafe { close(fd) };
    }

    #[test]
    fn socket_with_linux_type_flags_is_nonblocking_and_cloexec() {
        let lx_socket = wrapper!("socket", unsafe extern "C" fn(c_int, c_int, c_int) -> c_int);
        let ty = LX_SOCK_STREAM | LX_O_NONBLOCK | LX_O_CLOEXEC;
        let raw = unsafe { socket(LX_AF_INET, ty, 0) };
        assert_eq!(raw, -1, "host socket accepted Linux flags; control is void");

        let s = unsafe { lx_socket(LX_AF_INET, ty, 0) };
        assert!(s >= 0, "socket failed, engine errno {}", engine_errno());
        assert_ne!(host_fl(s) & FB_O_NONBLOCK, 0, "not non-blocking");
        assert_ne!(host_fd_flags(s) & FB_FD_CLOEXEC, 0, "not close-on-exec");
        unsafe { close(s) };
    }

    #[test]
    fn fcntl_setfl_linux_nonblock_takes_and_getfl_answers_in_linux_bits() {
        let lx_fcntl = wrapper!("fcntl", unsafe extern "C" fn(c_int, c_int, ...) -> c_int);
        let mut p = [0 as c_int; 2];
        assert_eq!(unsafe { pipe(p.as_mut_ptr()) }, 0);

        // Control: the host takes Linux O_NONBLOCK, reports success, and the
        // descriptor stays blocking -- the engine's self-pipe at startup.
        assert_eq!(unsafe { fcntl(p[0], FB_F_SETFL, LX_O_NONBLOCK) }, 0);
        assert_eq!(host_fl(p[0]) & FB_O_NONBLOCK, 0, "control: host set it after all");

        assert_eq!(unsafe { lx_fcntl(p[0], LX_F_SETFL, LX_O_NONBLOCK) }, 0);
        assert_ne!(host_fl(p[0]) & FB_O_NONBLOCK, 0, "wrapper left it blocking");
        let fl = unsafe { lx_fcntl(p[0], LX_F_GETFL) };
        assert_ne!(fl & LX_O_NONBLOCK, 0, "F_GETFL lost O_NONBLOCK: {fl:#x}");
        assert_eq!(fl & FB_O_NONBLOCK, 0, "F_GETFL answered in FreeBSD bits: {fl:#x}");

        // An undefined Linux bit is refused, not dropped.
        assert_eq!(unsafe { lx_fcntl(p[0], LX_F_SETFL, 0x0100_0000) }, -1);
        assert_eq!(engine_errno(), 22);
        unsafe {
            close(p[0]);
            close(p[1]);
        }
    }

    #[test]
    fn nonblocking_read_on_an_empty_pipe_reports_linux_eagain() {
        let lx_fcntl = wrapper!("fcntl", unsafe extern "C" fn(c_int, c_int, ...) -> c_int);
        let lx_read = wrapper!("read", unsafe extern "C" fn(c_int, *mut c_void, usize) -> isize);
        let mut p = [0 as c_int; 2];
        assert_eq!(unsafe { pipe(p.as_mut_ptr()) }, 0);
        assert_eq!(unsafe { lx_fcntl(p[0], LX_F_SETFL, LX_O_NONBLOCK) }, 0);
        let mut b = [0u8; 8];
        let r = unsafe { lx_read(p[0], b.as_mut_ptr().cast(), b.len()) };
        assert_eq!(r, -1);
        // Both routes an engine caller might read errno by must agree.
        assert_eq!(raw_errno(), LX_EAGAIN, "slot holds FreeBSD EAGAIN (35)");
        assert_eq!(engine_errno(), LX_EAGAIN, "__errno re-translated it");
        assert_eq!(engine_errno(), LX_EAGAIN, "second __errno read changed it");
        unsafe {
            close(p[0]);
            close(p[1]);
        }
    }

    #[test]
    fn errno_from_an_unwrapped_host_call_is_translated_by_errno() {
        // A host call nothing wraps leaves FreeBSD EAGAIN (35) in the slot;
        // `__errno` must hand the engine Linux's 11, once, and not then turn
        // that 11 into 35 (FreeBSD 11 is EDEADLK).
        unsafe { *__error() = 35 };
        assert_eq!(engine_errno(), LX_EAGAIN);
        assert_eq!(engine_errno(), LX_EAGAIN);
        unsafe { *__error() = 0 };
        assert_eq!(engine_errno(), 0);
    }

    #[test]
    fn nonblocking_connect_with_a_linux_sockaddr_reaches_a_listener() {
        let lx_socket = wrapper!("socket", unsafe extern "C" fn(c_int, c_int, c_int) -> c_int);
        let lx_connect =
            wrapper!("connect", unsafe extern "C" fn(c_int, *const c_void, u32) -> c_int);
        let lx_poll = wrapper!("poll", unsafe extern "C" fn(*mut PollFd, u64, c_int) -> c_int);
        let lx_getsockopt = wrapper!(
            "getsockopt",
            unsafe extern "C" fn(c_int, c_int, c_int, *mut c_void, *mut u32) -> c_int
        );
        let lx_getpeername =
            wrapper!("getpeername", unsafe extern "C" fn(c_int, *mut c_void, *mut u32) -> c_int);

        let (l, port) = host_listener(true);
        let sa = lx_sockaddr_in(port, [127, 0, 0, 1]);

        // Control: the host reads the Linux family bytes as sa_len=2,
        // sa_family=0 and refuses the address outright.
        let c = unsafe { socket(2, 1, 0) };
        assert_eq!(unsafe { connect(c, sa.as_ptr().cast(), 16) }, -1);
        unsafe { close(c) };

        let s = unsafe { lx_socket(LX_AF_INET, LX_SOCK_STREAM | LX_O_NONBLOCK, 0) };
        assert!(s >= 0);
        let r = unsafe { lx_connect(s, sa.as_ptr().cast(), 16) };
        if r != 0 {
            assert_eq!(engine_errno(), LX_EINPROGRESS, "connect: engine errno");
        }
        let mut pfd = PollFd { fd: s, events: LX_POLLOUT, revents: 0 };
        assert_eq!(unsafe { lx_poll(&mut pfd, 1, 5000) }, 1);
        assert_ne!(pfd.revents & LX_POLLOUT, 0);
        let mut err: c_int = -1;
        let mut len = 4u32;
        let rc = unsafe {
            lx_getsockopt(s, LX_SOL_SOCKET, LX_SO_ERROR, (&mut err as *mut c_int).cast(), &mut len)
        };
        assert_eq!(rc, 0);
        assert_eq!(err, 0, "SO_ERROR after a connect to a listener");

        let mut peer = [0u8; 16];
        let mut plen = 16u32;
        assert_eq!(unsafe { lx_getpeername(s, peer.as_mut_ptr().cast(), &mut plen) }, 0);
        assert_eq!(plen, 16);
        assert_eq!(u16::from_ne_bytes([peer[0], peer[1]]), LX_AF_INET as u16, "Linux family");
        assert_eq!(u16::from_ne_bytes([peer[2], peer[3]]), port);
        assert_eq!(&peer[4..8], &[127, 0, 0, 1]);
        unsafe {
            close(s);
            close(l);
        }
    }

    #[test]
    fn a_refused_nonblocking_connect_reports_linux_econnrefused() {
        let lx_socket = wrapper!("socket", unsafe extern "C" fn(c_int, c_int, c_int) -> c_int);
        let lx_connect =
            wrapper!("connect", unsafe extern "C" fn(c_int, *const c_void, u32) -> c_int);
        let lx_poll = wrapper!("poll", unsafe extern "C" fn(*mut PollFd, u64, c_int) -> c_int);
        let lx_getsockopt = wrapper!(
            "getsockopt",
            unsafe extern "C" fn(c_int, c_int, c_int, *mut c_void, *mut u32) -> c_int
        );

        // Bound but not listening: a SYN to it is answered with RST.
        let (b, port) = host_listener(false);
        let sa = lx_sockaddr_in(port, [127, 0, 0, 1]);
        let s = unsafe { lx_socket(LX_AF_INET, LX_SOCK_STREAM | LX_O_NONBLOCK, 0) };
        assert!(s >= 0);
        let r = unsafe { lx_connect(s, sa.as_ptr().cast(), 16) };
        let refused = if r == -1 && engine_errno() == LX_ECONNREFUSED {
            LX_ECONNREFUSED
        } else {
            assert_eq!(r, -1);
            assert_eq!(engine_errno(), LX_EINPROGRESS);
            let mut pfd = PollFd { fd: s, events: LX_POLLOUT, revents: 0 };
            assert_eq!(unsafe { lx_poll(&mut pfd, 1, 5000) }, 1);
            let mut err: c_int = -1;
            let mut len = 4u32;
            let rc = unsafe {
                lx_getsockopt(
                    s,
                    LX_SOL_SOCKET,
                    LX_SO_ERROR,
                    (&mut err as *mut c_int).cast(),
                    &mut len,
                )
            };
            assert_eq!(rc, 0);
            err
        };
        assert_eq!(refused, LX_ECONNREFUSED, "FreeBSD's is 61, Linux ENODATA");
        unsafe {
            close(s);
            close(b);
        }
    }

    #[test]
    fn getsockname_on_ipv6_reports_linux_af_inet6() {
        let lx_socket = wrapper!("socket", unsafe extern "C" fn(c_int, c_int, c_int) -> c_int);
        let lx_bind = wrapper!("bind", unsafe extern "C" fn(c_int, *const c_void, u32) -> c_int);
        let lx_getsockname =
            wrapper!("getsockname", unsafe extern "C" fn(c_int, *mut c_void, *mut u32) -> c_int);
        let s = unsafe { lx_socket(LX_AF_INET6, LX_SOCK_STREAM, 0) };
        if s < 0 {
            eprintln!("no IPv6 on this host (engine errno {}); skipped", engine_errno());
            return;
        }
        let mut sa = [0u8; 28];
        sa[0..2].copy_from_slice(&(LX_AF_INET6 as u16).to_ne_bytes());
        sa[23] = 1; // ::1
        if unsafe { lx_bind(s, sa.as_ptr().cast(), 28) } != 0 {
            eprintln!("cannot bind ::1 (engine errno {}); skipped", engine_errno());
            unsafe { close(s) };
            return;
        }
        let mut out = [0u8; 28];
        let mut len = 28u32;
        assert_eq!(unsafe { lx_getsockname(s, out.as_mut_ptr().cast(), &mut len) }, 0);
        assert_eq!(len, 28);
        assert_eq!(u16::from_ne_bytes([out[0], out[1]]), LX_AF_INET6 as u16);
        assert_eq!(out[23], 1);
        unsafe { close(s) };
    }

    #[test]
    fn open_with_linux_creat_trunc_creates_with_the_requested_mode() {
        let dir = std::env::temp_dir().join(format!("cordial-fbsd-abi-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let flags = LX_O_CREAT | LX_O_WRONLY | LX_O_TRUNC;

        // system_paths.cpp's `open`, which is what the engine's `open` resolves
        // to, and freebsd_abi.c's `openat`.
        let s_open = crate::bionic::system_path_overrides()
            .into_iter()
            .find(|(n, _)| *n == "open")
            .expect("system_paths registers open")
            .1;
        // SAFETY: s_open is `int (const char*, int, ...)`.
        let s_open: unsafe extern "C" fn(*const i8, c_int, ...) -> c_int =
            unsafe { std::mem::transmute(s_open) };
        let lx_openat =
            wrapper!("openat", unsafe extern "C" fn(c_int, *const i8, c_int, ...) -> c_int);

        for (which, name) in [("open", "a"), ("openat", "b")] {
            let path = dir.join(name);
            std::fs::write(&path, b"stale contents").unwrap();
            std::fs::remove_file(&path).unwrap();
            let c = CString::new(path.to_str().unwrap()).unwrap();
            let fd = unsafe {
                if which == "open" {
                    s_open(c.as_ptr(), flags, 0o640 as u32)
                } else {
                    lx_openat(-100, c.as_ptr(), flags, 0o640 as u32)
                }
            };
            assert!(fd >= 0, "{which}: failed, engine errno {}", engine_errno());
            unsafe { close(fd) };
            let meta = std::fs::metadata(&path).unwrap();
            assert_eq!(meta.permissions().mode() & 0o777, 0o640, "{which}: mode");

            // And O_TRUNC truncates an existing file. Untranslated, Linux
            // O_TRUNC (0x200) is FreeBSD O_CREAT: the open succeeds and the
            // stale contents stay.
            std::fs::write(&path, b"stale contents").unwrap();
            let fd = unsafe {
                if which == "open" {
                    s_open(c.as_ptr(), flags, 0o640 as u32)
                } else {
                    lx_openat(-100, c.as_ptr(), flags, 0o640 as u32)
                }
            };
            assert!(fd >= 0, "{which}: reopen failed, engine errno {}", engine_errno());
            unsafe { close(fd) };
            assert_eq!(std::fs::metadata(&path).unwrap().len(), 0, "{which}: not truncated");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_linux_open_flag_with_no_freebsd_meaning_is_refused() {
        let lx_openat =
            wrapper!("openat", unsafe extern "C" fn(c_int, *const i8, c_int, ...) -> c_int);
        let c = CString::new("/nonexistent-cordial").unwrap();
        // __O_TMPFILE: Linux callers fall back on EOPNOTSUPP (95).
        let fd = unsafe { lx_openat(-100, c.as_ptr(), 0x400000 | 0x10000 | 0x2, 0o600u32) };
        assert_eq!(fd, -1);
        assert_eq!(engine_errno(), 95);
        // A bit Linux does not define at all.
        let fd = unsafe { lx_openat(-100, c.as_ptr(), 0x0800_0000, 0u32) };
        assert_eq!(fd, -1);
        assert_eq!(engine_errno(), 22);
    }
}
