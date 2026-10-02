//! The arm64 guest's imports that M4's engine start-up reached
//! (docs/vr/dynarmic-design.md §9.3): threads as bionic makes them, signals
//! held virtually (§4), `setjmp`/`longjmp` on the guest's own registers,
//! `struct epoll_event` in arm64's layout, `ALooper` callbacks, the
//! `JNIEnv*`-taking NDK calls, and bionic's FORTIFY checks glibc does not
//! export.
//!
//! The same rule as `guest_libc`: each does what bionic would for an arm64
//! process or fails as the function documents, and none reports a success it
//! did not have.

use std::cell::Cell;
use std::collections::{BTreeSet, HashMap};
use std::ffi::{c_int, c_void};
use std::sync::{Arc, Mutex};

use cordial_guest::{guest_call, Fault, Handler, Ret, Runtime, StackSpec, Ty};

use crate::guest_libc::{count, errno_set, fortify_fatal};

const EINVAL: c_int = 22;

// ------------------------------------------------------------------ threads

extern "C" {
    fn pthread_attr_init(a: *mut c_void) -> c_int;
    fn pthread_attr_destroy(a: *mut c_void) -> c_int;
    fn pthread_attr_getdetachstate(a: *const c_void, s: *mut c_int) -> c_int;
    fn pthread_attr_setdetachstate(a: *mut c_void, s: c_int) -> c_int;
    fn pthread_attr_getstacksize(a: *const c_void, s: *mut usize) -> c_int;
    fn pthread_attr_getstackaddr(a: *const c_void, s: *mut *mut c_void) -> c_int;
    fn pthread_attr_setstack(a: *mut c_void, addr: *mut c_void, size: usize) -> c_int;
    fn pthread_attr_getinheritsched(a: *const c_void, s: *mut c_int) -> c_int;
    fn pthread_attr_setinheritsched(a: *mut c_void, s: c_int) -> c_int;
    fn pthread_attr_getschedpolicy(a: *const c_void, s: *mut c_int) -> c_int;
    fn pthread_attr_setschedpolicy(a: *mut c_void, s: c_int) -> c_int;
    fn pthread_attr_getschedparam(a: *const c_void, s: *mut c_int) -> c_int;
    fn pthread_attr_setschedparam(a: *mut c_void, s: *const c_int) -> c_int;
    fn pthread_getattr_np(t: u64, a: *mut c_void) -> c_int;
    fn pthread_create(
        t: *mut u64,
        attr: *const c_void,
        start: extern "C" fn(*mut c_void) -> *mut c_void,
        arg: *mut c_void,
    ) -> c_int;
}

/// bionic's default thread stack: 1 MiB less its signal stack
/// (`PTHREAD_STACK_SIZE_DEFAULT`), which is what a guest thread made with no
/// attr, or no stack size in it, gets on the Quest.
const BIONIC_DEFAULT_STACK: usize = (1 << 20) - (32 << 10);

/// A guest thread's stack and the host thread's attributes, read from the
/// guest's `pthread_attr_t`.
///
/// On an x86-64 host the guest's attr *is* glibc's: `pthread_attr_init` and
/// the setters dispatch to the host (`guest_link::FUNCS`), because bionic's
/// LP64 layout and glibc x86-64's are both 56 bytes and Cordial's native path
/// passes them through for the same reason (`bionic/pthread.rs`). So glibc's
/// getters read it. The host thread is given its own attr rather than the
/// guest's, because the guest's stack size describes guest frames: the host
/// thread runs the translator and the thunks, and keeps the host's default.
unsafe fn thread_attrs(guest: *const c_void, host: *mut c_void) -> StackSpec {
    // SAFETY: the caller's guarantee that `host` is an initialised attr and
    // `guest` null or one.
    unsafe {
        if guest.is_null() {
            return StackSpec::Size(BIONIC_DEFAULT_STACK);
        }
        let mut v = 0;
        if pthread_attr_getdetachstate(guest, &mut v) == 0 {
            pthread_attr_setdetachstate(host, v);
        }
        // An explicit scheduling policy is copied, and the host's answer to
        // it (EPERM for a real-time policy without the capability) is the
        // guest's, as it would be the kernel's on the Quest.
        if pthread_attr_getinheritsched(guest, &mut v) == 0 {
            pthread_attr_setinheritsched(host, v);
            if v == 1 {
                // PTHREAD_EXPLICIT_SCHED
                let mut p = 0;
                if pthread_attr_getschedpolicy(guest, &mut p) == 0 {
                    pthread_attr_setschedpolicy(host, p);
                }
                let mut prio = 0;
                if pthread_attr_getschedparam(guest, &mut prio) == 0 {
                    pthread_attr_setschedparam(host, &prio);
                }
            }
        }
        let mut size = 0usize;
        pthread_attr_getstacksize(guest, &mut size);
        let mut top: *mut c_void = std::ptr::null_mut();
        pthread_attr_getstackaddr(guest, &mut top);
        if !top.is_null() && size != 0 {
            // glibc keeps the top of a stack set by pthread_attr_setstack.
            StackSpec::Given(top as u64 - size as u64, size)
        } else if size != 0 {
            StackSpec::Size(size)
        } else {
            StackSpec::Size(BIONIC_DEFAULT_STACK)
        }
    }
}

/// `CORDIAL_GUEST_TRACE_THREADS=1`: a line per guest thread created.
fn trace_threads() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("CORDIAL_GUEST_TRACE_THREADS").is_some())
}

struct Start {
    rt: Arc<Runtime>,
    pc: u64,
    arg: u64,
    stack: StackSpec,
}

extern "C" fn thread_start(p: *mut c_void) -> *mut c_void {
    // SAFETY: the Box leaked by `pthread_create_guest`, handed over once.
    let s = unsafe { Box::from_raw(p as *mut Start) };
    cordial_guest::set_thread_stack(s.stack);
    let value = match guest_call(&s.rt, s.pc, &[s.arg], &[]) {
        Ok(r) => r.x0,
        Err(Fault::ThreadExit { value }) => value,
        // A guest thread has nobody to return a fault to. On Android the
        // process would take a signal and die; this does the same, named.
        Err(f) => thread_fault(s.pc, "start routine", &f),
    };
    if let Err(f) = crate::guest_libc::thread_exit(&s.rt) {
        thread_fault(s.pc, "thread exit", &f);
    }
    value as *mut c_void
}

fn thread_fault(pc: u64, when: &str, f: &Fault) -> ! {
    eprintln!("cordial-guest: guest thread started at {pc:#x} stopped in its {when}: {f:?}");
    for c in cordial_guest::last_fault_context() {
        eprintln!("  guest pc {:#x} lr {:#x} sp {:#x} frames {:x?}", c.pc, c.lr, c.sp, c.frames);
    }
    crate::guest_libc::report();
    std::process::abort();
}

/// `pthread_create(thread, attr, start, arg)`: a host thread whose first act
/// is to enter the guest at `start`, on a guest stack of the size the attr
/// asks for (or the guest's own memory, if it gave some), with its own Jit
/// (design §4). The attr's detach state and scheduling carry over to the
/// host thread.
fn pthread_create_guest() -> Handler {
    // `CORDIAL_GUEST_THREADS=0` is a control: every creation fails with
    // EAGAIN, which the kernel also returns when it is out of threads.
    let refuse = std::env::var("CORDIAL_GUEST_THREADS").as_deref() == Ok("0");
    Box::new(move |c| {
        count("pthread_create");
        if refuse {
            c.set_x(0, 11);
            return Ok(());
        }
        let mut host_attr = [0u64; 8];
        let ha = host_attr.as_mut_ptr() as *mut c_void;
        // SAFETY: a 64-byte buffer for glibc's 56-byte attr; the guest's attr
        // is null or one it initialised (see `thread_attrs`).
        let stack = unsafe {
            pthread_attr_init(ha);
            thread_attrs(c.x(1) as *const c_void, ha)
        };
        if trace_threads() {
            eprintln!("[guest] pthread_create start {:#x} arg {:#x} attr {:#x}: stack {stack:?}", c.x(2), c.x(3), c.x(1));
        }
        let s = Box::into_raw(Box::new(Start { rt: c.runtime().clone(), pc: c.x(2), arg: c.x(3), stack }));
        // glibc writes the new pthread_t through the guest's own pointer
        // before the thread runs, as bionic does. Copying it out afterwards
        // raced the new thread: engine threads that compare pthread_self()
        // with the handle their creator stored read zero, and a dispatcher
        // that then believed no thread was its own recursed on the main
        // thread until the guest stack ran out (§9.3).
        // SAFETY: `thread_start` takes ownership of `s` if and only if
        // creation succeeds; the guest's pthread_t* is 8 bytes, as glibc's.
        let r = unsafe { pthread_create(c.x(0) as *mut u64, ha, thread_start, s as *mut c_void) };
        // SAFETY: initialised above.
        unsafe { pthread_attr_destroy(ha) };
        if r != 0 {
            // SAFETY: not handed over, so still ours.
            drop(unsafe { Box::from_raw(s) });
        }
        c.set_x(0, r as u32 as u64);
        Ok(())
    })
}

/// `pthread_exit(value)`: leaves every guest frame on the thread by
/// travelling up as `Fault::ThreadExit` to `thread_start`, which runs the
/// thread's destructors and returns `value`. glibc's own `pthread_exit` is
/// never called: its forced unwind would meet the translator's frames, which
/// have no unwind information (design §4).
fn pthread_exit() -> Handler {
    Box::new(|c| {
        count("pthread_exit");
        Err(Fault::ThreadExit { value: c.x(0) })
    })
}

/// `pthread_getattr_np(thread, attr)`: the host's answer, with the stack
/// replaced by the thread's guest stack, which is the one the guest's frames
/// and its stack-overflow checks are about.
fn pthread_getattr_np_guest() -> Handler {
    Box::new(|c| {
        count("pthread_getattr_np");
        let (t, attr) = (c.x(0), c.x(1) as *mut c_void);
        // SAFETY: the guest's attr, which is glibc's layout on this host.
        let r = unsafe { pthread_getattr_np(t, attr) };
        if r == 0 {
            if let Some((lo, hi)) = cordial_guest::guest_stack_of(t) {
                // SAFETY: as above, and just initialised.
                unsafe { pthread_attr_setstack(attr, lo as *mut c_void, (hi - lo) as usize) };
            }
        }
        c.set_x(0, r as u32 as u64);
        Ok(())
    })
}

// ------------------------------------------------------------------ signals
//
// The host owns every real handler (design §4): dynarmic's fault handler
// chains to Rust's, and a guest handler installed for real would be jumped
// to as x86. So the guest's dispositions, masks and alternate stacks are
// recorded here and reported back exactly as set, and delivering a signal to
// a guest handler is not built: nothing here ever runs one. That is said
// once per signal on stderr, so a run that depends on delivery shows where.

/// arm64 bionic's `struct sigaction` (LP64): `sa_flags` (int, padded), the
/// handler, an 8-byte `sigset_t`, `sa_restorer`.
static DISPOSITIONS: Mutex<[[u64; 4]; 65]> = Mutex::new([[0; 4]; 65]);

thread_local! {
    /// The guest's signal mask on this thread; bionic's LP64 `sigset_t` is
    /// one word.
    static MASK: Cell<u64> = const { Cell::new(0) };
    /// The guest's `sigaltstack`, as `{ss_sp, ss_flags, ss_size}`.
    static ALTSTACK: Cell<[u64; 3]> = const { Cell::new([0, 2, 0]) };
}

fn announce(sig: u64, handler: u64) {
    static SAID: Mutex<u128> = Mutex::new(0);
    // SIG_DFL 0, SIG_IGN 1: nothing would ever run either way.
    if handler > 1 && sig < 128 {
        let mut s = SAID.lock().unwrap();
        if *s & (1 << sig) == 0 {
            *s |= 1 << sig;
            eprintln!("[guest] signal {sig}: guest handler {handler:#x} recorded; delivery to guest handlers \
                       is not built, so it never runs (design §4)");
        }
    }
}

/// `sigaction` on bionic's layout, shared with the raw `rt_sigaction`.
fn guest_sigaction(sig: u64, act: *const [u64; 4], old: *mut [u64; 4]) -> Result<(), c_int> {
    // SIGKILL and SIGSTOP cannot be caught, as the kernel says.
    if sig == 0 || sig > 64 || ((sig == 9 || sig == 19) && !act.is_null()) {
        return Err(EINVAL);
    }
    let mut d = DISPOSITIONS.lock().unwrap();
    if !old.is_null() {
        // SAFETY: the guest's struct sigaction out-parameter.
        unsafe { old.write(d[sig as usize]) };
    }
    if !act.is_null() {
        // SAFETY: the guest's struct sigaction.
        let a = unsafe { act.read() };
        d[sig as usize] = a;
        announce(sig, a[1]);
    }
    Ok(())
}

fn sigaction() -> Handler {
    Box::new(|c| {
        count("sigaction");
        match guest_sigaction(c.x(0), c.x(1) as *const [u64; 4], c.x(2) as *mut [u64; 4]) {
            Ok(()) => c.set_x(0, 0),
            Err(e) => {
                errno_set(e);
                c.set_x(0, u32::MAX as u64);
            }
        }
        Ok(())
    })
}

fn signal() -> Handler {
    Box::new(|c| {
        count("signal");
        let (sig, h) = (c.x(0), c.x(1));
        if sig == 0 || sig > 64 || sig == 9 || sig == 19 {
            errno_set(EINVAL);
            c.set_x(0, u64::MAX); // SIG_ERR
            return Ok(());
        }
        let mut d = DISPOSITIONS.lock().unwrap();
        let prev = d[sig as usize][1];
        // bionic's signal() is sigaction with SA_RESTART.
        d[sig as usize] = [0x1000_0000, h, 0, 0];
        announce(sig, h);
        c.set_x(0, prev);
        Ok(())
    })
}

fn sigset(fill: bool) -> Handler {
    Box::new(move |c| {
        let p = c.x(0) as *mut u64;
        if p.is_null() {
            errno_set(EINVAL);
            c.set_x(0, u32::MAX as u64);
            return Ok(());
        }
        // SAFETY: the guest's one-word sigset_t.
        unsafe { p.write(if fill { u64::MAX } else { 0 }) };
        c.set_x(0, 0);
        Ok(())
    })
}

/// The guest's per-thread mask; the answer is the errno, as pthread_sigmask
/// returns it.
fn guest_sigmask(how: u64, set: *const u64, old: *mut u64) -> Result<(), c_int> {
    let cur = MASK.with(|m| m.get());
    if !set.is_null() {
        // SAFETY: the guest's sigset_t.
        let s = unsafe { set.read() };
        let next = match how {
            0 => cur | s,  // SIG_BLOCK
            1 => cur & !s, // SIG_UNBLOCK
            2 => s,        // SIG_SETMASK
            _ => return Err(EINVAL),
        };
        // SIGKILL and SIGSTOP stay unblockable.
        MASK.with(|m| m.set(next & !((1 << 8) | (1 << 18))));
    }
    if !old.is_null() {
        // SAFETY: the guest's sigset_t out-parameter.
        unsafe { old.write(cur) };
    }
    Ok(())
}

fn pthread_sigmask() -> Handler {
    Box::new(|c| {
        count("pthread_sigmask");
        let r = guest_sigmask(c.x(0), c.x(1) as *const u64, c.x(2) as *mut u64);
        c.set_x(0, r.err().unwrap_or(0) as u64);
        Ok(())
    })
}

fn guest_sigaltstack(ss: *const [u64; 3], old: *mut [u64; 3]) {
    let cur = ALTSTACK.with(|a| a.get());
    if !ss.is_null() {
        // SAFETY: the guest's stack_t, laid out alike on both LP64 ABIs.
        let s = unsafe { ss.read() };
        ALTSTACK.with(|a| a.set(s));
    }
    if !old.is_null() {
        // SAFETY: the guest's stack_t out-parameter.
        unsafe { old.write(cur) };
    }
}

fn sigaltstack() -> Handler {
    Box::new(|c| {
        count("sigaltstack");
        guest_sigaltstack(c.x(0) as *const [u64; 3], c.x(1) as *mut [u64; 3]);
        c.set_x(0, 0);
        Ok(())
    })
}

// ------------------------------------------------------------ setjmp/longjmp

const JMP_MAGIC: u64 = 0x636f_7264_6a6d_7031; // "cordjmp1"

/// `setjmp(buf)`: the guest's callee-saved state at the call -- x19..x30, SP,
/// d8..d15 -- into its own `jmp_buf` (bionic's is 32 words), with this
/// thread and re-entry depth, so a `longjmp` that would cross a host frame is
/// refused rather than performed.
fn setjmp() -> Handler {
    extern "C" {
        fn pthread_self() -> u64;
    }
    Box::new(|c| {
        count("setjmp");
        let b = c.x(0) as *mut u64;
        // SAFETY: the guest's jmp_buf, 32 words on arm64 bionic; 23 used.
        unsafe {
            b.write(JMP_MAGIC);
            b.add(1).write(pthread_self() ^ ((cordial_guest::thread_depth() as u64) << 56));
            for i in 0..12 {
                b.add(2 + i).write(c.x(19 + i as u32));
            }
            b.add(14).write(c.sp());
            for i in 0..8 {
                b.add(15 + i).write(c.v(8 + i as u32)[0]);
            }
        }
        c.set_x(0, 0);
        Ok(())
    })
}

/// `longjmp(buf, val)`: restores what `setjmp` saved and returns `val` (1
/// for 0) from it -- the stub's own `ret` goes to the saved x30.
fn longjmp() -> Handler {
    extern "C" {
        fn pthread_self() -> u64;
    }
    Box::new(|c| {
        count("longjmp");
        let b = c.x(0) as *const u64;
        // SAFETY: the guest's jmp_buf, written by `setjmp` above if the magic
        // word matches.
        let (magic, who) = unsafe { (b.read(), b.add(1).read()) };
        // SAFETY: plain libc call.
        let me = unsafe { pthread_self() } ^ ((cordial_guest::thread_depth() as u64) << 56);
        if magic != JMP_MAGIC || who != me {
            return Err(Fault::Unsupported {
                thunk: "longjmp".into(),
                why: if magic != JMP_MAGIC {
                    "the jmp_buf was not filled by setjmp".into()
                } else {
                    "the setjmp was on another thread or re-entry depth: the jump would cross a host frame".into()
                },
            });
        }
        // SAFETY: as above.
        unsafe {
            for i in 0..12 {
                c.set_x(19 + i as u32, b.add(2 + i).read());
            }
            c.set_sp(b.add(14).read());
            for i in 0..8 {
                c.set_v(8 + i as u32, [b.add(15 + i).read(), 0]);
            }
        }
        let v = c.x(1) as u32;
        c.set_x(0, if v == 0 { 1 } else { v as u64 });
        Ok(())
    })
}

// -------------------------------------------------------------------- epoll
//
// `struct epoll_event` is `{u32 events; u64 data}`: 16 bytes on arm64, 12 on
// x86-64, where the kernel declares it packed (`linux/eventpoll.h`).

/// The host's layout, as `android/looper.rs` declares it.
#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
struct HostEpollEvent {
    events: u32,
    data: u64,
}

fn epoll_ctl() -> Handler {
    extern "C" {
        fn epoll_ctl(epfd: c_int, op: c_int, fd: c_int, ev: *mut HostEpollEvent) -> c_int;
    }
    Box::new(|c| {
        count("epoll_ctl");
        let g = c.x(3) as *const u8;
        let mut h = HostEpollEvent::default();
        let hp = if g.is_null() {
            std::ptr::null_mut()
        } else {
            // SAFETY: the guest's 16-byte epoll_event.
            unsafe {
                h.events = (g as *const u32).read_unaligned();
                h.data = (g.add(8) as *const u64).read_unaligned();
            }
            &mut h as *mut HostEpollEvent
        };
        // SAFETY: plain syscall wrapper with a packed event or null.
        let r = unsafe { epoll_ctl(c.x(0) as c_int, c.x(1) as c_int, c.x(2) as c_int, hp) };
        c.set_x(0, r as u32 as u64);
        Ok(())
    })
}

fn epoll_wait() -> Handler {
    extern "C" {
        fn epoll_wait(epfd: c_int, evs: *mut HostEpollEvent, max: c_int, timeout: c_int) -> c_int;
    }
    Box::new(|c| {
        count("epoll_wait");
        let (epfd, out, max, timeout) = (c.x(0) as c_int, c.x(1) as *mut u8, c.x(2) as c_int, c.x(3) as c_int);
        let mut buf = vec![HostEpollEvent::default(); max.max(1) as usize];
        // SAFETY: a buffer of `max` packed events; the kernel checks max.
        let r = unsafe { epoll_wait(epfd, buf.as_mut_ptr(), max, timeout) };
        for (i, e) in buf.iter().take(r.max(0) as usize).enumerate() {
            let (events, data) = (e.events, e.data);
            // SAFETY: the guest's array of `max` 16-byte events; r <= max.
            unsafe {
                let g = out.add(16 * i);
                (g as *mut u32).write_unaligned(events);
                (g.add(4) as *mut u32).write_unaligned(0);
                (g.add(8) as *mut u64).write_unaligned(data);
            }
        }
        c.set_x(0, r as u32 as u64);
        Ok(())
    })
}

// ------------------------------------------------------------------ ALooper

/// `ALooper_addFd(looper, fd, ident, events, callback, data)`, with a guest
/// callback behind a host entry (`int (*)(int fd, int events, void* data)`)
/// so Cordial's looper can call it as its own. One entry per distinct
/// callback, made once.
fn alooper_add_fd(native: usize) -> Handler {
    static ENTRIES: Mutex<Option<HashMap<u64, usize>>> = Mutex::new(None);
    Box::new(move |c| {
        count("ALooper_addFd");
        let cb = c.x(4);
        let host_cb = if cb == 0 {
            0
        } else {
            let mut m = ENTRIES.lock().unwrap();
            let m = m.get_or_insert_with(HashMap::new);
            match m.get(&cb) {
                Some(&e) => e as u64,
                None => {
                    let e = cordial_guest::host_entry(c.runtime(), "ALooper callback", cb,
                                                      vec![Ty::I32, Ty::I32, Ty::Ptr], Ret::Int(Ty::I32), None)
                        .map_err(|why| Fault::Unsupported { thunk: "ALooper_addFd".into(), why })?;
                    m.insert(cb, e as usize);
                    e as u64
                }
            }
        };
        let vals = [c.x(0), c.x(1), c.x(2), c.x(3), host_cb, c.x(5)];
        // SAFETY: Cordial's ALooper_addFd, with the guest's own arguments and
        // a host-callable callback.
        let out = unsafe {
            cordial_guest::invoke("ALooper_addFd", native as *const c_void,
                                  &[Ty::Ptr, Ty::I32, Ty::I32, Ty::I32, Ty::Ptr, Ty::Ptr], &vals)
        }?;
        c.set_x(0, out.rax as u32 as u64);
        Ok(())
    })
}

/// An NDK call whose first argument is a `JNIEnv*`: the guest's is swapped
/// for the calling thread's host env.
fn with_host_env(name: &'static str, native: usize, args: &'static [Ty], ret: Ret) -> Handler {
    Box::new(move |c| {
        count(name);
        let mut vals = cordial_guest::collect(c, args);
        vals[0] = crate::guest_jni::host_env_for(vals[0]).ok_or_else(|| Fault::Unsupported {
            thunk: name.into(),
            why: "the JNIEnv* is not the guest's, or this thread has no host env".into(),
        })?;
        // SAFETY: Cordial's implementation of `name`, with its C types.
        let out = unsafe { cordial_guest::invoke(name, native as *const c_void, args, &vals) }?;
        cordial_guest::write_ret(c, ret, &out);
        Ok(())
    })
}

// ------------------------------------------------------------------ FORTIFY

/// bionic's `__FD_SET_chk`/`__FD_CLR_chk`/`__FD_ISSET_chk(fd, set, size)`,
/// which glibc does not export: the fd must be in `[0, FD_SETSIZE)` and the
/// set at least an `fd_set` (128 bytes), or the process aborts.
fn fd_chk(name: &'static str, op: u8) -> Handler {
    Box::new(move |c| {
        let (fd, set, size) = (c.x(0) as i32, c.x(1) as *mut u64, c.x(2));
        if !(0..1024).contains(&fd) {
            return Err(fortify_fatal(name, &format!("file descriptor {fd} is < 0 or >= FD_SETSIZE")));
        }
        if size < 128 {
            return Err(fortify_fatal(name, &format!("set is too small: {size} < 128")));
        }
        let (w, bit) = ((fd / 64) as usize, 1u64 << (fd % 64));
        // SAFETY: the guest's fd_set, at least 128 bytes by the check above.
        unsafe {
            let p = set.add(w);
            match op {
                b's' => p.write(p.read() | bit),
                b'c' => p.write(p.read() & !bit),
                _ => c.set_x(0, (p.read() & bit != 0) as u64),
            }
        }
        Ok(())
    })
}

/// `__sendto_chk(fd, buf, len, buflen, flags, addr, addrlen)`.
fn sendto_chk() -> Handler {
    extern "C" {
        fn sendto(fd: c_int, buf: *const c_void, len: usize, flags: c_int, addr: *const c_void, al: u32) -> isize;
    }
    Box::new(|c| {
        count("__sendto_chk");
        if c.x(2) > c.x(3) {
            return Err(fortify_fatal("sendto", &format!("prevented {}-byte read from {}-byte buffer",
                                                          c.x(2), c.x(3))));
        }
        // SAFETY: plain libc call with the guest's buffer and address.
        let r = unsafe {
            sendto(c.x(0) as c_int, c.x(1) as *const c_void, c.x(2) as usize, c.x(4) as c_int,
                   c.x(5) as *const c_void, c.x(6) as u32)
        };
        c.set_x(0, r as u64);
        Ok(())
    })
}

/// `ioctl(fd, request, arg)` for the requests whose numbers and argument
/// layouts arm64 and x86-64 share: the generic `FIO*`/`TIOCGWINSZ` numbers
/// (x86 uses the asm-generic ones for these) and the `SIOCGIF*` interface
/// queries, whose `struct ifreq` (40 bytes) and `struct ifconf` (16) are
/// the same on both LP64 ABIs. Any other request stops, naming its number.
fn ioctl() -> Handler {
    extern "C" {
        fn ioctl(fd: c_int, req: u64, ...) -> c_int;
    }
    const SHARED: &[(u64, &str)] = &[
        (0x5421, "FIONBIO"), (0x541b, "FIONREAD"), (0x5451, "FIOCLEX"), (0x5450, "FIONCLEX"),
        (0x5413, "TIOCGWINSZ"), (0x8912, "SIOCGIFCONF"), (0x8913, "SIOCGIFFLAGS"),
        (0x8915, "SIOCGIFADDR"), (0x8919, "SIOCGIFBRDADDR"), (0x891b, "SIOCGIFNETMASK"),
        (0x8921, "SIOCGIFMTU"), (0x8927, "SIOCGIFHWADDR"), (0x8933, "SIOCGIFINDEX"),
        (0x8910, "SIOCGIFNAME"),
    ];
    Box::new(|c| {
        count("ioctl");
        let req = c.x(1) & 0xffff_ffff;
        if !SHARED.iter().any(|(n, _)| *n == req) {
            return Err(Fault::Unsupported {
                thunk: "ioctl".into(),
                why: format!("request {req:#x} is not one whose number and argument arm64 and x86-64 share"),
            });
        }
        // SAFETY: a request from the table above, whose argument is an int,
        // an int*, or a structure laid out alike on both.
        let r = unsafe { ioctl(c.x(0) as c_int, req, c.x(2)) };
        c.set_x(0, r as u32 as u64);
        Ok(())
    })
}

/// glibc's negative `EAI_*` as bionic's positive ones, as
/// `native/netdb_compat.cpp` maps them for `getaddrinfo`.
fn eai_to_bionic(r: c_int) -> c_int {
    match r {
        0 => 0,
        -1 => 3,    // BADFLAGS
        -2 => 8,    // NONAME
        -3 => 2,    // AGAIN
        -4 => 4,    // FAIL
        -5 => 7,    // NODATA
        -6 => 5,    // FAMILY
        -7 => 10,   // SOCKTYPE
        -8 => 9,    // SERVICE
        -9 => 1,    // ADDRFAMILY
        -10 => 6,   // MEMORY
        -11 => 11,  // SYSTEM
        -12 => 14,  // OVERFLOW
        _ => 4,     // FAIL: a code bionic has no name for
    }
}

/// `getnameinfo(sa, salen, host, hostlen, serv, servlen, flags)`. bionic's
/// `NI_*` are BSD's (NOFQDN 1, NUMERICHOST 2, NAMEREQD 4, NUMERICSERV 8,
/// DGRAM 16) and glibc's another order (NUMERICHOST 1, NUMERICSERV 2,
/// NOFQDN 4, NAMEREQD 8, DGRAM 16), and the `EAI_*` results differ in sign.
/// A flag bionic does not define fails with EAI_BADFLAGS, as bionic's does.
fn getnameinfo() -> Handler {
    extern "C" {
        fn getnameinfo(sa: *const c_void, salen: u32, host: *mut u8, hl: u32, serv: *mut u8, sl: u32,
                       flags: c_int) -> c_int;
    }
    Box::new(|c| {
        count("getnameinfo");
        let f = c.x(6) as u32;
        if f & !0x1f != 0 {
            c.set_x(0, 3);
            return Ok(());
        }
        let map = [(1, 4), (2, 1), (4, 8), (8, 2), (16, 16)];
        let host_flags = map.iter().filter(|(b, _)| f & b != 0).fold(0, |a, (_, h)| a | h);
        // SAFETY: the guest's sockaddr and buffers, with its lengths.
        let r = unsafe {
            getnameinfo(c.x(0) as *const c_void, c.x(1) as u32, c.x(2) as *mut u8, c.x(3) as u32,
                        c.x(4) as *mut u8, c.x(5) as u32, host_flags)
        };
        c.set_x(0, eai_to_bionic(r) as u32 as u64);
        Ok(())
    })
}

/// `mallinfo()`: bionic's `struct mallinfo` is ten `size_t`s (80 bytes), so
/// AAPCS64 returns it through the memory x8 points at. What it describes is
/// the libc heap, which here is glibc's; `mallinfo2` reports it in the same
/// ten words. The engine's own allocator is not libc's on either side.
fn mallinfo() -> Handler {
    #[repr(C)]
    struct Mallinfo2([usize; 10]);
    extern "C" {
        fn mallinfo2() -> Mallinfo2;
    }
    Box::new(|c| {
        count("mallinfo");
        // SAFETY: plain libc call; x8 is the caller's 80-byte result slot.
        unsafe { (c.x(8) as *mut [usize; 10]).write(mallinfo2().0) };
        Ok(())
    })
}

/// `ldiv(n, d)`: `{long quot; long rem}` comes back in x0 and x1.
fn ldiv() -> Handler {
    Box::new(|c| {
        let (n, d) = (c.x(0) as i64, c.x(1) as i64);
        if d == 0 {
            return Err(Fault::Unsupported { thunk: "ldiv".into(), why: "division by zero".into() });
        }
        c.set_x(0, n.wrapping_div(d) as u64);
        c.set_x(1, n.wrapping_rem(d) as u64);
        Ok(())
    })
}

/// bionic's `strerror_r` is POSIX's (`int`), which glibc exports as
/// `__xpg_strerror_r`.
fn strerror_r() -> Handler {
    extern "C" {
        fn __xpg_strerror_r(e: c_int, buf: *mut u8, n: usize) -> c_int;
    }
    Box::new(|c| {
        // SAFETY: the guest's buffer of the size it gave.
        let r = unsafe { __xpg_strerror_r(c.x(0) as c_int, c.x(1) as *mut u8, c.x(2) as usize) };
        c.set_x(0, r as u32 as u64);
        Ok(())
    })
}

// ------------------------------------------------------------ raw syscalls
//
// Two ways reach the kernel with arm64's numbering (asm-generic/unistd.h):
// bionic's `syscall(nr, ...)`, an import, and the guest's own `svc #0`, which
// the engine also executes directly (libroblox+0x32b8978, 2.740.927). Both
// come here, so a number means the same thing whichever way it arrives.
//
// Calls whose arguments mean the same on both kernels -- scalars, and
// pointers to structures both LP64 kernels lay out alike -- are renumbered to
// x86-64's (arch/x86/entry/syscalls/syscall_64.tbl) and made for real. Calls
// that change state the translator owns go to the same virtual answers the
// libc imports get: the guest's memory map, which the Jits' translated code is
// keyed by, and its signal dispositions, masks and alternate stacks, which are
// recorded and never installed (design §4). `openat` goes where `open` does,
// with arm64's O_* renumbered. Thread creation, exit and
// `rt_sigreturn` have no virtual answer that could be given from a raw
// syscall and are refused by name. Anything else fails with ENOSYS, which is
// what a kernel says of a call it does not implement, named once on stderr so
// the gap is visible.
const SYSCALLS: &[(u64, u64, &str)] = &[
    (17, 79, "getcwd"), (23, 32, "dup"), (43, 137, "statfs"), (44, 138, "fstatfs"), (24, 292, "dup3"), (29, 16, "ioctl_unused"),
    (57, 3, "close"), (59, 293, "pipe2"), (62, 8, "lseek"), (63, 0, "read"), (64, 1, "write"),
    (65, 19, "readv"), (66, 20, "writev"), (67, 17, "pread64"), (68, 18, "pwrite64"),
    (82, 74, "fsync"), (83, 75, "fdatasync"), (98, 202, "futex"), (99, 273, "set_robust_list"),
    (100, 274, "get_robust_list"), (101, 35, "nanosleep"), (113, 228, "clock_gettime"),
    (114, 229, "clock_getres"), (115, 230, "clock_nanosleep"), (118, 142, "sched_setparam"),
    (119, 144, "sched_setscheduler"), (120, 145, "sched_getscheduler"), (121, 143, "sched_getparam"),
    (122, 203, "sched_setaffinity"), (123, 204, "sched_getaffinity"), (124, 24, "sched_yield"),
    (125, 146, "sched_get_priority_max"), (126, 147, "sched_get_priority_min"), (130, 200, "tkill"),
    (131, 234, "tgkill"), (140, 141, "setpriority"), (141, 140, "getpriority"),
    (165, 98, "getrusage"), (168, 309, "getcpu"), (169, 96, "gettimeofday"), (172, 39, "getpid"),
    (173, 110, "getppid"), (174, 102, "getuid"), (175, 107, "geteuid"), (176, 104, "getgid"),
    (177, 108, "getegid"), (178, 186, "gettid"), (179, 99, "sysinfo"),
    (228, 149, "mlock"), (229, 150, "munlock"), (241, 298, "perf_event_open"),
    (270, 310, "process_vm_readv"), (271, 311, "process_vm_writev"), (278, 318, "getrandom"),
    (279, 319, "memfd_create"), (283, 324, "membarrier"), (291, 332, "statx"), (293, 334, "rseq"),
];

/// Numbers answered here rather than passed through: those that change
/// state the translator owns, and `openat`, whose flags are renumbered and
/// whose paths go through Cordial's path layer.
const OWN_ANSWER: &[(u64, &str)] = &[
    (56, "openat"), (93, "exit"), (94, "exit_group"), (96, "set_tid_address"), (132, "sigaltstack"),
    (134, "rt_sigaction"), (135, "rt_sigprocmask"), (139, "rt_sigreturn"), (215, "munmap"),
    (220, "clone"), (222, "mmap"), (226, "mprotect"), (233, "madvise"), (435, "clone3"),
];

const ENOSYS: i64 = 38;

/// The arm64 name of a syscall number this module knows, for the log.
fn syscall_name(nr: u64) -> &'static str {
    SYSCALLS.iter().find(|e| e.0 == nr).map(|e| e.2)
        .or_else(|| OWN_ANSWER.iter().find(|e| e.0 == nr).map(|e| e.1))
        .unwrap_or("?")
}

/// The x86-64 kernel's own answer, a negative errno on failure. Not glibc's
/// `syscall()`, so the caller's errno is never touched on the way: a raw
/// `svc #0` does not set it either.
unsafe fn host_syscall(nr: u64, a: [u64; 6]) -> i64 {
    let r: i64;
    // SAFETY: the caller's; the syscall instruction clobbers only rcx and r11.
    unsafe {
        std::arch::asm!("syscall", inlateout("rax") nr as i64 => r, in("rdi") a[0], in("rsi") a[1],
                        in("rdx") a[2], in("r10") a[3], in("r8") a[4], in("r9") a[5],
                        lateout("rcx") _, lateout("r11") _, options(nostack));
    }
    r
}

/// arm64 syscall `nr` with `a`: the kernel-style answer (a negative errno on
/// failure), `None` for a number with no translation, or a fault for one
/// that cannot return.
pub(crate) fn arm64_syscall(nr: u64, a: [u64; 6], site: u64) -> Result<Option<i64>, Fault> {
    use crate::guest_libc::guest_openat;
    use cordial_guest::code;
    let raw = |r: Result<u64, c_int>| Some(r.map_or_else(|e| -(e as i64), |v| v as i64));
    let unit = |r: Result<(), c_int>| Some(r.map_or_else(|e| -(e as i64), |()| 0));
    Ok(match nr {
        // openat: the path layer and arm64's O_* numbering, as `open` gets.
        56 => raw(guest_openat(a[0] as i64, a[1], a[2] as u32, a[3] as u32)?),
        222 => raw(code::mmap(a[0], a[1], a[2], a[3], a[4], a[5], site)?),
        215 => raw(code::munmap(a[0], a[1], site)?),
        226 => raw(code::mprotect(a[0], a[1], a[2], site)?),
        233 => raw(code::madvise(a[0], a[1], a[2] as c_int, site)?),
        // The kernel's arm64 `struct sigaction` is {handler, flags, restorer,
        // mask}; bionic's, which the virtual table holds, is {flags, handler,
        // mask, restorer}.
        134 => {
            if a[3] != 8 {
                return Ok(Some(-(EINVAL as i64)));
            }
            let (act, old) = (a[1] as *const [u64; 4], a[2] as *mut [u64; 4]);
            // SAFETY: the guest's kernel-layout struct sigaction, when given.
            let b = (!act.is_null()).then(|| unsafe { act.read() }).map(|k| [k[1], k[0], k[3], k[2]]);
            let mut prev = [0u64; 4];
            let r = guest_sigaction(a[0], b.as_ref().map_or(std::ptr::null(), |b| b as *const _),
                                    if old.is_null() { std::ptr::null_mut() } else { &mut prev });
            if r.is_ok() && !old.is_null() {
                // SAFETY: the guest's out-parameter, kernel layout.
                unsafe { old.write([prev[1], prev[0], prev[3], prev[2]]) };
            }
            unit(r)
        }
        135 => {
            if a[3] != 8 {
                return Ok(Some(-(EINVAL as i64)));
            }
            unit(guest_sigmask(a[0], a[1] as *const u64, a[2] as *mut u64))
        }
        132 => {
            guest_sigaltstack(a[0] as *const [u64; 3], a[1] as *mut [u64; 3]);
            Some(0)
        }
        94 => return Err(Fault::Exit { function: "exit_group (svc #0)".into(), status: a[0] as i32 }),
        93 | 139 => return Err(Fault::Unsupported {
            thunk: format!("svc #0 {}", syscall_name(nr)),
            why: "leaves the guest's frames without a return, which only pthread_exit and a delivered \
                  signal can do here (design §4)".into(),
        }),
        // A thread made by a raw clone would have no Jit to run it; the
        // guest's threads are pthread_create's. And the host thread's
        // clear-tid word is glibc's, not the guest's to move.
        220 | 435 | 96 => {
            refuse(nr, &a, "creates or retargets a thread outside pthread_create");
            Some(-ENOSYS)
        }
        _ => match SYSCALLS.iter().find(|e| e.0 == nr) {
            // SAFETY: a call whose six arguments mean the same on both
            // kernels, by the table above.
            Some(&(_, host, _)) => Some(unsafe { host_syscall(host, a) }),
            None => None,
        },
    })
}

fn refuse(nr: u64, a: &[u64; 6], why: &str) {
    static NAMED: Mutex<BTreeSet<u64>> = Mutex::new(BTreeSet::new());
    if NAMED.lock().unwrap().insert(nr) {
        eprintln!("[guest] syscall {nr} ({}) refused with ENOSYS: {why}; args (hex) {a:x?}", syscall_name(nr));
    }
}

/// Names an untranslated number once, whichever way it arrived.
pub(crate) fn name_untranslated(via: &str, nr: u64, a: &[u64; 6]) {
    static NAMED: Mutex<BTreeSet<u64>> = Mutex::new(BTreeSet::new());
    if NAMED.lock().unwrap().insert(nr) {
        eprintln!("[guest] {via}: syscall {nr} with arm64 numbering has no translation; ENOSYS (args, hex, {a:x?})");
    }
}

fn svc_log() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("CORDIAL_GUEST_SVC_LOG").is_some())
}

/// The handler for the guest's own `svc #0` (`Runtime::set_syscall_handler`):
/// x8 the number, x0..x5 the arguments, the kernel's answer back in x0. The
/// first call of each number is logged with its arguments and PC;
/// `CORDIAL_GUEST_SVC_LOG=1` logs every call.
pub fn raw_syscall() -> Handler {
    static SEEN: Mutex<BTreeSet<u64>> = Mutex::new(BTreeSet::new());
    Box::new(|c| {
        count("svc #0");
        let nr = c.x(8);
        let a = [c.x(0), c.x(1), c.x(2), c.x(3), c.x(4), c.x(5)];
        let pc = c.pc().wrapping_sub(4);
        let first = SEEN.lock().unwrap().insert(nr);
        let r = arm64_syscall(nr, a, pc)?.unwrap_or_else(|| {
            name_untranslated("svc #0", nr, &a);
            -ENOSYS
        });
        if first || svc_log() {
            // SAFETY: plain syscall.
            let tid = unsafe { host_syscall(186, [0; 6]) };
            // SAFETY: openat's path argument, a guest C string.
            let path = if nr == 56 && a[1] != 0 {
                format!(" path {:?}", unsafe { std::ffi::CStr::from_ptr(a[1] as *const std::ffi::c_char) })
            } else {
                String::new()
            };
            eprintln!("[guest] svc #0 at {pc:#x} on {tid}: nr {nr} ({}) x0..x5 (hex) {:x?}{path} -> {r}",
                      syscall_name(nr), a);
        }
        c.set_x(0, r as u64);
        Ok(())
    })
}

/// The thunk for `name`, if this module has one.
pub fn handler(name: &str, _rt: &Arc<Runtime>, native_of: &dyn Fn(&str) -> Option<usize>)
    -> Option<(Handler, &'static str)>
{
    use Ty::Ptr;
    let native = native_of(name);
    Some(match name {
        "pthread_create" => (pthread_create_guest(),
                             "a host thread entering the guest, attr's stack size, detach state and scheduling kept"),
        "pthread_exit" => (pthread_exit(), "leaves the guest frames, then runs the thread's destructors"),
        "pthread_getattr_np" => (pthread_getattr_np_guest(), "the host's, describing the guest stack"),
        "sigaction" => (sigaction(), "recorded virtually, never installed (design §4)"),
        "signal" => (signal(), "recorded virtually, never installed (design §4)"),
        "sigemptyset" => (sigset(false), "bionic's one-word sigset_t"),
        "sigfillset" => (sigset(true), "bionic's one-word sigset_t"),
        "pthread_sigmask" => (pthread_sigmask(), "the guest's mask, per thread, virtual"),
        "sigaltstack" => (sigaltstack(), "the guest's, per thread, virtual"),
        "setjmp" => (setjmp(), "the guest's callee-saved registers"),
        "longjmp" => (longjmp(), "the guest's callee-saved registers; refuses to cross a host frame"),
        "epoll_ctl" => (epoll_ctl(), "the host's, epoll_event unpacked to arm64's 16 bytes"),
        "epoll_wait" => (epoll_wait(), "the host's, epoll_event unpacked to arm64's 16 bytes"),
        "ALooper_addFd" => (alooper_add_fd(native?), "Cordial's, the guest callback behind a host entry"),
        "AAssetManager_fromJava" => (with_host_env("AAssetManager_fromJava", native?, &[Ptr, Ptr], Ret::Int(Ptr)),
                                     "Cordial's, with the host JNIEnv"),
        "ANativeWindow_fromSurface" => (with_host_env("ANativeWindow_fromSurface", native?, &[Ptr, Ptr],
                                                      Ret::Int(Ptr)), "Cordial's, with the host JNIEnv"),
        "__FD_SET_chk" => (fd_chk("__FD_SET_chk", b's'), "bionic's FORTIFY check, then the bit"),
        "__FD_CLR_chk" => (fd_chk("__FD_CLR_chk", b'c'), "bionic's FORTIFY check, then the bit"),
        "__FD_ISSET_chk" => (fd_chk("__FD_ISSET_chk", b'i'), "bionic's FORTIFY check, then the bit"),
        "__sendto_chk" => (sendto_chk(), "bionic's FORTIFY check, then the host's sendto"),
        "strerror_r" => (strerror_r(), "POSIX strerror_r, glibc's __xpg_strerror_r"),
        "mallinfo" => (mallinfo(), "glibc's mallinfo2 into bionic's ten size_t fields, returned through x8"),
        "ldiv" => (ldiv(), "quotient and remainder in x0 and x1"),
        "getnameinfo" => (getnameinfo(), "the host's, NI_* and EAI_* renumbered for bionic"),
        "ioctl" => (ioctl(), "the host's, for requests numbered and laid out alike; others stop by number"),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn rt() -> Arc<Runtime> {
        Runtime::new(cordial_guest::Options::default())
    }

    fn stub(rt: &Arc<Runtime>, name: &str) -> u64 {
        let (h, _) = handler(name, rt, &|_| None).unwrap_or_else(|| panic!("{name} has no thunk"));
        rt.register(name, h)
    }

    extern "C" {
        #[link_name = "pthread_join"]
        fn host_join(t: u64, out: *mut *mut c_void) -> c_int;
    }

    /// A stub is guest-callable code, so one can be a guest thread's start
    /// routine: this one records the stack the thread's guest state got.
    static SEEN_STACK: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn pthread_create_honours_the_attr_stack_size_and_writes_the_handle_first() {
        let rt = rt();
        let create = stub(&rt, "pthread_create");
        let start = rt.register("record", Box::new(|c| {
            let (lo, hi) = cordial_guest::thread_guest_stack().unwrap();
            SEEN_STACK.store(hi - lo, Ordering::SeqCst);
            c.set_x(0, 0x5eed);
            Ok(())
        }));
        let mut attr = [0u64; 8];
        let mut handle = 0u64;
        // SAFETY: a glibc attr, as the guest's is on this host.
        unsafe {
            pthread_attr_init(attr.as_mut_ptr().cast());
            extern "C" {
                fn pthread_attr_setstacksize(a: *mut c_void, s: usize) -> c_int;
            }
            pthread_attr_setstacksize(attr.as_mut_ptr().cast(), 256 << 10);
        }
        let r = cordial_guest::guest_call(&rt, create, &[&mut handle as *mut u64 as u64, attr.as_ptr() as u64, start, 0],
                                          &[]).unwrap();
        assert_eq!(r.x0, 0);
        assert_ne!(handle, 0);
        let mut out = std::ptr::null_mut();
        // SAFETY: a joinable thread just created.
        assert_eq!(unsafe { host_join(handle, &mut out) }, 0);
        assert_eq!(out as u64, 0x5eed);
        assert_eq!(SEEN_STACK.load(Ordering::SeqCst), 256 << 10);
    }

    #[test]
    fn pthread_exit_returns_its_value_to_join() {
        let rt = rt();
        let create = stub(&rt, "pthread_create");
        let exit = stub(&rt, "pthread_exit");
        // The start routine is pthread_exit itself, entered with arg in x0.
        let mut handle = 0u64;
        let r = cordial_guest::guest_call(&rt, create, &[&mut handle as *mut u64 as u64, 0, exit, 0xfeed], &[]).unwrap();
        assert_eq!(r.x0, 0);
        let mut out = std::ptr::null_mut();
        // SAFETY: as above.
        assert_eq!(unsafe { host_join(handle, &mut out) }, 0);
        assert_eq!(out as u64, 0xfeed);
    }

    #[test]
    fn epoll_events_cross_in_arm64s_layout() {
        extern "C" {
            fn epoll_create1(f: c_int) -> c_int;
            fn eventfd(v: u32, f: c_int) -> c_int;
            fn write(fd: c_int, b: *const c_void, n: usize) -> isize;
            fn close(fd: c_int) -> c_int;
        }
        let rt = rt();
        let (ctl, wait) = (stub(&rt, "epoll_ctl"), stub(&rt, "epoll_wait"));
        // SAFETY: plain syscalls on descriptors this test owns.
        let (ep, ev) = unsafe { (epoll_create1(0), eventfd(0, 0)) };
        // arm64's struct epoll_event: events, 4 bytes of padding, data.
        let mut g = [0u8; 16];
        g[..4].copy_from_slice(&1u32.to_ne_bytes()); // EPOLLIN
        g[4..8].copy_from_slice(&[0xaa; 4]);
        g[8..].copy_from_slice(&0x1122_3344_5566_7788u64.to_ne_bytes());
        let r = cordial_guest::guest_call(&rt, ctl, &[ep as u64, 1, ev as u64, g.as_ptr() as u64], &[]).unwrap();
        assert_eq!(r.x0 as i32, 0);
        // SAFETY: an 8-byte eventfd increment.
        unsafe { write(ev, &1u64 as *const u64 as *const c_void, 8) };
        let mut out = [0xffu8; 32];
        let r = cordial_guest::guest_call(&rt, wait, &[ep as u64, out.as_mut_ptr() as u64, 2, 0], &[]).unwrap();
        assert_eq!(r.x0 as i32, 1);
        assert_eq!(u32::from_ne_bytes(out[..4].try_into().unwrap()), 1);
        assert_eq!(&out[4..8], &[0; 4]);
        assert_eq!(u64::from_ne_bytes(out[8..16].try_into().unwrap()), 0x1122_3344_5566_7788);
        assert_eq!(&out[16..], &[0xff; 16], "only one event written");
        // SAFETY: closing this test's descriptors.
        unsafe {
            close(ev);
            close(ep);
        }
    }

    /// A disposition is kept and reported back exactly, and SIGKILL cannot
    /// be caught.
    #[test]
    fn sigaction_is_recorded_and_reported_back() {
        let rt = rt();
        let sa = stub(&rt, "sigaction");
        let act = [0x0800_0004u64, 0xdead_beef, 1 << 9, 0];
        let mut old = [0u64; 4];
        let r = cordial_guest::guest_call(&rt, sa, &[10, act.as_ptr() as u64, 0], &[]).unwrap();
        assert_eq!(r.x0, 0);
        let r = cordial_guest::guest_call(&rt, sa, &[10, 0, old.as_mut_ptr() as u64], &[]).unwrap();
        assert_eq!(r.x0, 0);
        assert_eq!(old, act);
        let r = cordial_guest::guest_call(&rt, sa, &[9, act.as_ptr() as u64, 0], &[]).unwrap();
        assert_eq!(r.x0 as i32, -1);
    }

    #[test]
    fn getnameinfo_flags_and_results_are_bionics() {
        let rt = rt();
        let gni = stub(&rt, "getnameinfo");
        // 127.0.0.1:80, with bionic's NI_NUMERICHOST|NI_NUMERICSERV (2|8).
        let mut sa = [0u8; 16];
        sa[..2].copy_from_slice(&2u16.to_ne_bytes());
        sa[2..4].copy_from_slice(&80u16.to_be_bytes());
        sa[4..8].copy_from_slice(&[127, 0, 0, 1]);
        let (mut host, mut serv) = ([0u8; 64], [0u8; 16]);
        let r = cordial_guest::guest_call(&rt, gni, &[sa.as_ptr() as u64, 16, host.as_mut_ptr() as u64, 64,
                                                      serv.as_mut_ptr() as u64, 16, 2 | 8], &[]).unwrap();
        assert_eq!(r.x0, 0);
        assert!(host.starts_with(b"127.0.0.1\0"));
        assert!(serv.starts_with(b"80\0"));
        // A flag bionic does not define is EAI_BADFLAGS, bionic's 3.
        let r = cordial_guest::guest_call(&rt, gni, &[sa.as_ptr() as u64, 16, host.as_mut_ptr() as u64, 64,
                                                      0, 0, 0x100], &[]).unwrap();
        assert_eq!(r.x0, 3);
    }

    #[test]
    fn each_arm64_syscall_is_mapped_once() {
        let mut seen = BTreeSet::new();
        for (a, _, n) in SYSCALLS {
            assert!(seen.insert(*a), "{n} ({a}) twice");
        }
        for (a, n) in OWN_ANSWER {
            assert!(seen.insert(*a), "{n} ({a}) is both passed through and owned");
        }
    }

    /// `mov x8, #nr; svc #0; ret`, the shape of the engine's own raw calls.
    fn raw_call(nr: u32) -> cordial_guest::Mapping {
        let words = [0xd280_0008 | (nr << 5), 0xd400_0001, 0xd65f_03c0u32];
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        cordial_guest::Mapping::with_contents(&bytes, 0)
    }

    #[test]
    fn raw_svc_stops_without_a_handler_and_is_answered_with_one() {
        let code = raw_call(172); // getpid
        let bare = rt();
        match guest_call(&bare, code.addr(), &[], &[]) {
            Err(Fault::Syscall { pc }) => assert_eq!(pc, code.addr() + 4),
            other => panic!("expected Fault::Syscall, got {other:?}"),
        }
        let rt = rt();
        rt.set_syscall_handler(raw_syscall());
        let r = guest_call(&rt, code.addr(), &[], &[]).unwrap();
        assert_eq!(r.x0, std::process::id() as u64);
        // A number with no translation is the kernel's ENOSYS, as -38 in x0.
        let unknown = raw_call(999);
        assert_eq!(guest_call(&rt, unknown.addr(), &[], &[]).unwrap().x0 as i64, -38);
        assert_eq!(rt.syscall_count(), 2);
    }

    #[test]
    fn raw_rt_sigaction_is_the_virtual_table_in_the_kernels_layout() {
        // Kernel arm64 layout: handler, flags, restorer, mask.
        let act: [u64; 4] = [0x1234_5678, 0x0400_0004, 0x9abc, 1 << 9];
        let mut old = [0u64; 4];
        let r = arm64_syscall(134, [12, &act as *const _ as u64, 0, 8, 0, 0], 0).unwrap().unwrap();
        assert_eq!(r, 0);
        // bionic's, as the sigaction import reads it back: flags, handler, mask, restorer.
        let mut bionic = [0u64; 4];
        guest_sigaction(12, std::ptr::null(), &mut bionic).unwrap();
        assert_eq!(bionic, [0x0400_0004, 0x1234_5678, 1 << 9, 0x9abc]);
        arm64_syscall(134, [12, 0, &mut old as *mut _ as u64, 8, 0, 0], 0).unwrap().unwrap();
        assert_eq!(old, act);
        // A sigset size other than arm64's 8 is EINVAL, as the kernel says.
        assert_eq!(arm64_syscall(134, [12, 0, 0, 16, 0, 0], 0).unwrap(), Some(-22));
        // Thread creation is refused, never made.
        assert_eq!(arm64_syscall(220, [0; 6], 0).unwrap(), Some(-38));
        assert!(matches!(arm64_syscall(94, [3, 0, 0, 0, 0, 0], 0), Err(Fault::Exit { status: 3, .. })));
    }

    #[test]
    fn raw_openat_and_fstatfs_answer_as_the_kernel_does() {
        extern "C" {
            #[link_name = "open"]
            fn host_open(p: *const std::ffi::c_char, f: c_int, ...) -> c_int;
        }
        // Relative to a directory the test opened, which is the host's
        // openat: the native path layer is only linked in a real client.
        // SAFETY: a literal path.
        let root = unsafe { host_open(c"/".as_ptr(), 0) } as u64;
        let path = c"proc/self/maps";
        let fd = arm64_syscall(56, [root, path.as_ptr() as u64, 0, 0, 0, 0], 0).unwrap().unwrap();
        assert!(fd >= 0, "openat -> {fd}");
        let mut buf = [0u64; 15]; // arm64's struct statfs, 120 bytes
        assert_eq!(arm64_syscall(44, [fd as u64, buf.as_mut_ptr() as u64, 0, 0, 0, 0], 0).unwrap(), Some(0));
        assert_eq!(buf[0], 0x9fa0, "f_type is PROC_SUPER_MAGIC");
        assert_eq!(arm64_syscall(57, [fd as u64, 0, 0, 0, 0, 0], 0).unwrap(), Some(0));
        // O_DIRECTORY is 0o40000 on arm64 and 0o200000 on x86-64: renumbered,
        // so opening a file with it fails as ENOTDIR rather than succeeding.
        let r = arm64_syscall(56, [root, path.as_ptr() as u64, 0o40000, 0, 0, 0], 0).unwrap().unwrap();
        assert_eq!(r, -20);
        // With no native open, a path the working directory resolves stops by
        // name rather than bypassing the path layer. Asked of `openat_via`
        // directly: `NATIVE_OPEN` is process-wide, and any test in this binary
        // that links a guest library sets it, which made the same assertion
        // through `arm64_syscall` fail in CI whenever that test ran first.
        let cwd = crate::guest_libc::openat_via("open", None, -100, c"/x".as_ptr() as u64, 0, 0);
        assert!(cwd.is_err());
    }
}
