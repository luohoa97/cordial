//! Optional logging wrappers around the libc calls that reveal what Roblox is
//! trying to do.
//!
//! There is no `strace` in the target environment and the process has no frame
//! pointers, so an unwound backtrace past the innermost frame is guesswork.
//! Cordial owns the symbol table, though, which means any import can be
//! intercepted. That is a better instrument than a debugger here: it says what
//! was called, with what, in order.
//!
//! Enabled with `CORDIAL_TRACE=1`. Off by default, and **not merely because it
//! is loud**: `open64`, `openat`, `prctl` and `syscall` are variadic, and these
//! wrappers declare them with fixed arity. That is not ABI-safe — the callee
//! reads `al` for the vector-register count and walks the register save area
//! differently — and turning this on has been observed to make Roblox abort
//! where it otherwise runs. Treat any behaviour seen under it as suspect until
//! reproduced without it.
//!
//! `CORDIAL_ANDROID_TRACE=1` is the safe counterpart: the Android API has no
//! variadic entry points, so wrapping it changes nothing.

use super::{cstr as s, die_with};
use std::ffi::{c_char, c_int, c_long, c_void};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static ENABLED: AtomicBool = AtomicBool::new(false);
static SEQ: AtomicU64 = AtomicU64::new(0);

pub fn enable() {
    ENABLED.store(true, Ordering::Relaxed);
}

fn log(args: std::fmt::Arguments<'_>) {
    if ENABLED.load(Ordering::Relaxed) {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        // The thread id is not decoration. Roblox runs this work on twenty-odd
        // threads, and a single interleaved sequence invites reading two
        // unrelated calls as cause and effect.
        eprintln!("[{n:>6}] tid={} {args}", current_tid());
    }
}

fn current_tid() -> c_int {
    // SAFETY: takes no arguments and reads this thread's own id.
    unsafe { libc_gettid() }
}

unsafe extern "C" {
    #[link_name = "gettid"]
    fn libc_gettid() -> c_int;
}

// `s` is `super::cstr`: two copies of the null-then-`CStr` conversion for
// diagnostics, one here and one for `__assert`, are now one in `ffi_util`.

unsafe extern "C" {
    fn open64(path: *const c_char, flags: c_int, ...) -> c_int;
    fn openat(dirfd: c_int, path: *const c_char, flags: c_int, ...) -> c_int;
    fn access(path: *const c_char, mode: c_int) -> c_int;
    fn getenv(name: *const c_char) -> *mut c_char;
    fn sysconf(name: c_int) -> c_long;
    fn getauxval(kind: c_ulong) -> c_ulong;
    fn prctl(option: c_int, ...) -> c_int;
    fn syscall(num: c_long, ...) -> c_long;
}

#[allow(non_camel_case_types)]
type c_ulong = u64;

// ------------------------------------------------------------------- wrappers

unsafe extern "C" fn t_open64(path: *const c_char, flags: c_int, mode: u32) -> c_int {
    // SAFETY: forwarding the caller's own arguments.
    let fd = unsafe { open64(path, flags, mode) };
    log(format_args!("open64({:?}, {flags:#x}) = {fd}", unsafe { s(path) }));
    fd
}

unsafe extern "C" fn t_openat(dirfd: c_int, path: *const c_char, flags: c_int, mode: u32) -> c_int {
    // SAFETY: forwarding the caller's own arguments.
    let fd = unsafe { openat(dirfd, path, flags, mode) };
    log(format_args!("openat({dirfd}, {:?}, {flags:#x}) = {fd}", unsafe { s(path) }));
    fd
}

unsafe extern "C" fn t_access(path: *const c_char, mode: c_int) -> c_int {
    // SAFETY: forwarding the caller's own arguments.
    let rc = unsafe { access(path, mode) };
    log(format_args!("access({:?}, {mode}) = {rc}", unsafe { s(path) }));
    rc
}

unsafe extern "C" fn t_getenv(name: *const c_char) -> *mut c_char {
    // SAFETY: forwarding the caller's own arguments.
    let v = unsafe { getenv(name) };
    log(format_args!(
        "getenv({:?}) = {:?}",
        unsafe { s(name) },
        unsafe { s(v) }
    ));
    v
}

unsafe extern "C" fn t_sysconf(name: c_int) -> c_long {
    // SAFETY: forwarding the caller's own arguments.
    let v = unsafe { sysconf(name) };
    log(format_args!("sysconf({name}) = {v}"));
    v
}

unsafe extern "C" fn t_getauxval(kind: c_ulong) -> c_ulong {
    // SAFETY: forwarding the caller's own arguments.
    let v = unsafe { getauxval(kind) };
    log(format_args!("getauxval({kind}) = {v:#x}"));
    v
}

unsafe extern "C" fn t_prctl(option: c_int, a: u64, b: u64, c: u64, d: u64) -> c_int {
    // SAFETY: forwarding the caller's own arguments.
    let rc = unsafe { prctl(option, a, b, c, d) };
    log(format_args!("prctl({option}, {a:#x}, ...) = {rc}"));
    rc
}

unsafe extern "C" fn t_syscall(num: c_long, a: u64, b: u64, c: u64, d: u64, e: u64, f: u64) -> c_long {
    // SAFETY: forwarding the caller's own arguments.
    let rc = unsafe { syscall(num, a, b, c, d, e, f) };
    log(format_args!("syscall({num}, {a:#x}, {b:#x}) = {rc}"));
    rc
}

/// What `abort()` prints. The one wrapper that is worth having even without
/// `--trace`: it turns "the process died" into "the process chose to die, here".
const ABORT_REPORT: &str = "\n*** Roblox called abort() ***";

const STACK_CHK_REPORT: &str = "\n*** stack protector tripped inside Roblox ***\n    \
     A callee wrote past its frame. If Cordial provided that callee,\n    \
     its signature or a struct layout is wrong — see bionic::pthread.";

/// What `__android_log_assert` prints, as one block.
fn log_assert_report(tag: &str, cond: &str, msg: &str) -> String {
    format!(
        "\n*** Roblox log-assert ***\n    tag:       {tag}\n    condition: {cond}\n    message:   {msg}"
    )
}

// All three end in `super::die_with`, which prints, lists the stubs hit so far
// and calls the host's `abort` through `std::process::abort` -- the same
// `abort(3)` the extern declaration above reached, without an `unsafe` block to
// say so.

unsafe extern "C" fn t_abort() -> ! {
    die_with(ABORT_REPORT)
}

unsafe extern "C" fn t_stack_chk_fail() -> ! {
    die_with(STACK_CHK_REPORT)
}

unsafe extern "C" fn t_android_log_assert(
    cond: *const c_char,
    tag: *const c_char,
    fmt: *const c_char,
) -> ! {
    // SAFETY: liblog's contract is NUL-terminated strings or null.
    let (tag, cond, msg) = unsafe { (s(tag), s(cond), s(fmt)) };
    die_with(&log_assert_report(&tag, &cond, &msg))
}

/// Wrappers installed unconditionally, because a silent abort costs more time
/// than the wrapper costs anything.
pub fn always_on() -> Vec<(&'static str, *mut c_void)> {
    macro_rules! f {
        ($name:literal, $fn:expr) => {
            ($name, $fn as *const () as *mut c_void)
        };
    }
    vec![
        f!("abort", t_abort),
        f!("__stack_chk_fail", t_stack_chk_fail),
        f!("__android_log_assert", t_android_log_assert),
    ]
}

/// Wrappers installed only with `--trace`.
pub fn verbose() -> Vec<(&'static str, *mut c_void)> {
    macro_rules! f {
        ($name:literal, $fn:expr) => {
            ($name, $fn as *const () as *mut c_void)
        };
    }
    vec![
        f!("open64", t_open64),
        f!("openat", t_openat),
        f!("access", t_access),
        f!("getenv", t_getenv),
        f!("sysconf", t_sysconf),
        f!("getauxval", t_getauxval),
        f!("prctl", t_prctl),
        f!("syscall", t_syscall),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_log_assert_prints_tag_condition_and_message_in_that_order() {
        assert_eq!(
            log_assert_report("RobloxEngine", "ptr != nullptr", "no device"),
            "\n*** Roblox log-assert ***\n    tag:       RobloxEngine\n    condition: ptr != nullptr\n    message:   no device"
        );
    }

    #[test]
    fn the_stack_protector_report_names_the_likely_culprit() {
        // Four lines once the banner is split: the point of the message is the
        // pointer to `bionic::pthread`, so a reflow must not lose it.
        assert!(STACK_CHK_REPORT.contains("stack protector tripped"));
        assert!(STACK_CHK_REPORT.contains("bionic::pthread"));
        assert_eq!(STACK_CHK_REPORT.lines().count(), 4);
        assert!(STACK_CHK_REPORT.lines().skip(2).all(|l| l.starts_with("    ")), "{STACK_CHK_REPORT}");
    }

    #[test]
    fn the_always_on_wrappers_are_exactly_the_three_that_end_the_process() {
        let names: Vec<_> = always_on().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["abort", "__stack_chk_fail", "__android_log_assert"]);
    }

    #[test]
    fn the_verbose_set_still_holds_the_variadic_wrappers_it_is_off_by_default_for() {
        // `CORDIAL_TRACE=1` is unsafe because these are declared with fixed
        // arity; if one were dropped from the table the warning in the module
        // doc would be about nothing, and if one were converted this test is
        // the prompt to reread it.
        let names: Vec<_> = verbose().into_iter().map(|(n, _)| n).collect();
        for variadic in ["open64", "openat", "prctl", "syscall"] {
            assert!(names.contains(&variadic), "{variadic}");
        }
    }

    #[test]
    fn a_thread_id_is_positive() {
        assert!(current_tid() > 0);
    }

    #[test]
    fn logging_is_silent_until_enabled() {
        // `log` advances the sequence only when tracing is on, so a disabled
        // run cannot be told from one that never called it.
        let before = SEQ.load(Ordering::Relaxed);
        if !ENABLED.load(Ordering::Relaxed) {
            log(format_args!("must not be counted"));
            assert_eq!(SEQ.load(Ordering::Relaxed), before);
        }
    }
}
