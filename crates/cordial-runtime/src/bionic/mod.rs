//! Symbols Cordial implements itself, because neither a host library nor a stub
//! is right.
//!
//! Three kinds live here:
//!
//! * **bionic-only entry points** with a host equivalent under a different name
//!   (`__errno` is glibc's `__errno_location`).
//! * **FORTIFY wrappers** (`__strlen_chk` and friends). bionic compiles these in
//!   at `-D_FORTIFY_SOURCE`; each is its unchecked counterpart plus a bound the
//!   caller already knows. Forwarding loses the check, not the behaviour.
//! * **diagnostics that must not silently succeed.** `__assert2` is the one that
//!   matters: as a stub returning 0, a failed assertion inside Roblox continues
//!   with corrupt state and fails somewhere unrelated later. That is exactly the
//!   phantom-state debugging the estimation guidance warns about.
//!
//! Everything here is deliberately small. The real bionic surface is a shim's
//! job; see docs/base-evaluation.md §4.

use crate::ffi_util::{borrow_request, lossy_or_null, write_response};
use std::ffi::{c_char, c_int, c_void, CStr};

pub mod pthread;
pub mod signal;
pub mod trace;

/// Functions Cordial provides. Consulted before any host library.
pub fn function_overrides() -> Vec<(&'static str, *mut c_void)> {
    macro_rules! f {
        ($name:literal, $fn:expr) => {
            ($name, $fn as *const () as *mut c_void)
        };
    }
    let mut v = vec![
        f!("__errno", bionic_errno),
        f!("__assert", bionic_assert),
        f!("__assert2", bionic_assert2),
        f!("__gnu_strerror_r", gnu_strerror_r),
        f!("strerror_r", bionic_strerror_r),
        // FORTIFY wrappers.
        f!("__strlen_chk", strlen_chk),
        f!("__strchr_chk", strchr_chk),
        f!("__strrchr_chk", strrchr_chk),
        f!("__strncpy_chk2", strncpy_chk2),
        f!("__write_chk", write_chk),
        f!("__umask_chk", umask_chk),
        f!("__poll_chk", poll_chk),
        f!("__readlink_chk", readlink_chk),
        f!("__fread_chk", fread_chk),
        // Android system properties. Desktop values, so Roblox stops believing
        // it is on a phone. Framework-layer policy, implemented here because the
        // native side reads it through libc.
        f!("__system_property_get", system_property_get),
        // See each function's own comment for the divergence it translates.
        f!("mallinfo", bionic_mallinfo),
        f!("__cxa_thread_atexit_impl", bionic_cxa_thread_atexit_impl),
        // Stubbed until now, and a stub here answers 0 — which reads as an
        // impossibly old Android rather than an unknown one.
        f!("android_get_device_api_level", android_get_device_api_level),
        // Selector numbering differs wholesale between the two libcs.
        f!("sysconf", bionic_sysconf),
        f!("pathconf", bionic_pathconf),
        // Plain ctype, but the Quest build's platform loader is the first thing
        // to import it, and as a Generic libc symbol it resolved only under
        // `--host-libc`. Implemented rather than forwarded; see the function.
        f!("isxdigit", bionic_isxdigit),
        // The legacy `__sF` streams. Zeroed storage below stops the load-time
        // failure; these make a write through `&__sF[k]` reach the host's real
        // stream instead of faulting on a zeroed FILE. Three separate engine
        // calls were segfaulting at `0x8` inside `_IO_fflush` before this --
        // docs/analysis/flag-init.md §18.
        f!("fflush", legacy_fflush),
        f!("fclose", legacy_fclose),
        f!("fseek", legacy_fseek),
        f!("ftell", legacy_ftell),
        f!("fputs", legacy_fputs),
        f!("setvbuf", legacy_setvbuf),
        f!("fread", legacy_fread),
        f!("fwrite", legacy_fwrite),
        f!("fprintf", legacy_fprintf),
        f!("vfprintf", legacy_vfprintf),
    ];
    // sigset_t is 8 bytes in bionic and 128 in glibc; struct sigaction is 32
    // against 152, with a different field order. Passing either through is a
    // 120-byte overrun of the caller's object.
    v.extend(signal::overrides());
    // Synchronisation primitives whose bionic layout differs from glibc's.
    v.extend(pthread::overrides());
    // Android's liblog — Roblox's own account of what it is doing.
    v.extend(liblog_overrides());
    // Android's `/system` tree. These are path-taking libc calls, redirected
    // only for paths under `/system`; everything else forwards untouched.
    v.extend(system_path_overrides());
    // `struct addrinfo` has its last two pointers swapped between bionic and
    // glibc, and the `AI_*` constants disagree outright. Same class as the
    // `sigset_t` and `pthread_mutex_t` translations above.
    v.extend(netdb_overrides());
    // `vasprintf` and `getcwd(NULL, n)` return a host-`malloc`'d block that the
    // engine would free through its own allocator. Refused rather than answered
    // -- see `alloc_overrides` and `native/foreign_alloc.cpp`.
    v.extend(alloc_overrides());
    // OpenSL ES. Data symbols, so a missing one fails the DT_NEEDED walk rather
    // than the first audio call — see `opensles_overrides`.
    v.extend(opensles_overrides());
    // `pthread_create`, forwarded untouched unless `CORDIAL_TRACE_THREADS=1` —
    // see `thread_overrides` and `native/thread_trace.cpp`.
    v.extend(thread_overrides());
    if std::env::var_os("CORDIAL_TRACE_PATHS").is_some() {
        unsafe extern "C" {
            fn cordial_set_path_trace(on: c_int);
        }
        // SAFETY: sets one bool in system_paths.cpp.
        unsafe { cordial_set_path_trace(1) };
    }
    if std::env::var_os("CORDIAL_TRACE_THREADS").is_some() {
        unsafe extern "C" {
            fn cordial_set_thread_trace(on: c_int);
        }
        // SAFETY: sets one bool in thread_trace.cpp.
        unsafe { cordial_set_thread_trace(1) };
    }
    // A silent abort costs more debugging time than these wrappers cost anything.
    v.extend(trace::always_on());
    if std::env::var_os("CORDIAL_TRACE").is_some() {
        trace::enable();
        v.extend(trace::verbose());
    }
    v
}

/// Data symbols. The *address* is what matters, not a call.
pub fn data_overrides() -> Vec<(&'static str, *mut c_void)> {
    vec![
        (
            "__stack_chk_guard",
            std::ptr::addr_of!(STACK_CHK_GUARD) as *mut c_void,
        ),
        ("__sF", std::ptr::addr_of!(LEGACY_SF) as *mut c_void),
    ]
}

unsafe extern "C" {
    #[link_name = "cordial_legacy_fflush"]
    fn legacy_fflush(f: *mut c_void) -> c_int;
    #[link_name = "cordial_legacy_fclose"]
    fn legacy_fclose(f: *mut c_void) -> c_int;
    #[link_name = "cordial_legacy_fseek"]
    fn legacy_fseek(f: *mut c_void, off: i64, whence: c_int) -> c_int;
    #[link_name = "cordial_legacy_ftell"]
    fn legacy_ftell(f: *mut c_void) -> i64;
    #[link_name = "cordial_legacy_fputs"]
    fn legacy_fputs(s: *const c_char, f: *mut c_void) -> c_int;
    #[link_name = "cordial_legacy_setvbuf"]
    fn legacy_setvbuf(f: *mut c_void, buf: *mut c_char, mode: c_int, size: usize) -> c_int;
    #[link_name = "cordial_legacy_fread"]
    fn legacy_fread(p: *mut c_void, sz: usize, n: usize, f: *mut c_void) -> usize;
    #[link_name = "cordial_legacy_fwrite"]
    fn legacy_fwrite(p: *const c_void, sz: usize, n: usize, f: *mut c_void) -> usize;
    #[link_name = "cordial_legacy_fprintf"]
    fn legacy_fprintf(f: *mut c_void, fmt: *const c_char, ...) -> c_int;
    #[link_name = "cordial_legacy_vfprintf"]
    fn legacy_vfprintf(f: *mut c_void, fmt: *const c_char, ap: *mut c_void) -> c_int;
}

/// bionic's pre-API-23 `FILE __sF[3]`, where `stdout` was the macro `&__sF[1]`.
///
/// Roblox targets SDK 35 and reaches the standard streams through the modern
/// `stdin`/`stdout`/`stderr` pointer variables, which resolve to the host's
/// directly — both libcs store a `FILE*` in a variable of that name, so the
/// indirection matches and no translation is needed. Something statically linked
/// into libroblox.so was nonetheless compiled against the older headers and
/// still imports `__sF`.
///
/// It must at least be readable memory. As a function-pointer stub it is not:
/// glibc walks its stream list at process exit, finds the stub address where a
/// FILE should be, and reports "invalid stdio handle" after everything else has
/// already succeeded.
///
/// Zeroed storage stops the crash. It does **not** make the legacy streams work
/// — anything that actually writes through `&__sF[k]` needs each FILE*-taking
/// function wrapped to remap the pointer onto the host's real stream, which is
/// only worth building once something is observed using it.
static LEGACY_SF: [[u8; LEGACY_FILE_SIZE]; 3] = [[0; LEGACY_FILE_SIZE]; 3];

/// The array's address and stride, for `native/legacy_stdio.cpp`.
///
/// Exported rather than restated on that side because `LEGACY_FILE_SIZE` is a
/// number nobody can derive twice reliably, and two copies that drift would send
/// a legacy write to an address a third of a FILE off the stream it meant.
#[unsafe(no_mangle)]
pub extern "C" fn cordial_legacy_sf_base() -> *const u8 {
    std::ptr::addr_of!(LEGACY_SF) as *const u8
}

#[unsafe(no_mangle)]
pub extern "C" fn cordial_legacy_sf_stride() -> usize {
    LEGACY_FILE_SIZE
}

/// `sizeof(struct __sFILE)` in pre-M bionic on LP64. Only the total matters: the
/// array has to span the addresses a legacy caller would compute.
const LEGACY_FILE_SIZE: usize = 152;

/// The stack canary. Its value is arbitrary as long as it is stable for the
/// process: the function prologue and epilogue compare against the same word.
/// Low byte zero is the usual convention — it terminates string copies.
static STACK_CHK_GUARD: usize = 0x0011_2233_4455_6600usize.to_le();

/// The canary's value. An arm64 guest reads it from TLS slot 5 as well as
/// through the `__stack_chk_guard` import, and bionic keeps the two equal;
/// so does the translator, which is why it asks here.
pub fn stack_chk_guard() -> u64 {
    STACK_CHK_GUARD as u64
}

unsafe extern "C" {
    fn __errno_location() -> *mut c_int;
    fn strlen(s: *const c_char) -> usize;
    fn strchr(s: *const c_char, c: c_int) -> *mut c_char;
    fn strrchr(s: *const c_char, c: c_int) -> *mut c_char;
    fn strncpy(dst: *mut c_char, src: *const c_char, n: usize) -> *mut c_char;
    // glibc's `strerror_r` is the GNU one: it returns `char*` and may leave
    // `buf` untouched. The XSI one bionic's plain `strerror_r` means is
    // exported under the other name.
    #[link_name = "strerror_r"]
    fn glibc_gnu_strerror_r(errnum: c_int, buf: *mut c_char, buflen: usize) -> *mut c_char;
    fn __xpg_strerror_r(errnum: c_int, buf: *mut u8, buflen: usize) -> c_int;
    fn write(fd: c_int, buf: *const c_void, count: usize) -> isize;
    fn umask(mode: u32) -> u32;
    fn poll(fds: *mut c_void, nfds: u64, timeout: c_int) -> c_int;
    fn readlink(path: *const c_char, buf: *mut c_char, bufsize: usize) -> isize;
    fn fread(ptr: *mut c_void, size: usize, n: usize, stream: *mut c_void) -> usize;
    #[link_name = "dlsym"]
    fn libc_dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    #[link_name = "__cxa_thread_atexit_impl"]
    fn host_cxa_thread_atexit_impl(
        func: *mut c_void,
        arg: *mut c_void,
        dso_handle: *mut c_void,
    ) -> c_int;
}

/// bionic's `struct mallinfo`: ten `size_t`, where glibc's is ten `int`.
///
/// Eighty bytes against forty on LP64, and every field the wrong width and at
/// the wrong offset. The engine imports `mallinfo` undefined, so before this it
/// resolved to a stub; letting it reach glibc's would have been worse than the
/// stub, which is the point `native/system_paths.cpp` makes about `statvfs`.
///
/// glibc has exactly the right shape under another name. `mallinfo2` is the
/// same ten fields as `size_t`, added in glibc 2.33 precisely because the `int`
/// version overflows on large heaps -- so the translation is a field-for-field
/// copy rather than a widening, and there is no truncation anywhere in it.
#[repr(C)]
pub struct BionicMallinfo {
    arena: usize,
    ordblks: usize,
    smblks: usize,
    hblks: usize,
    hblkhd: usize,
    usmblks: usize,
    fsmblks: usize,
    uordblks: usize,
    fordblks: usize,
    keepcost: usize,
}

/// glibc's `mallinfo2`, looked up at run time rather than linked.
///
/// `dlsym` rather than an `extern` block so a host older than glibc 2.33 gets
/// zeroes instead of a link failure. Zeroes are the honest answer there: the
/// only other source is the `int` `mallinfo`, and copying its fields into
/// `size_t` slots would report a heap over 2 GB as whatever it wrapped to.
fn host_mallinfo2() -> Option<extern "C" fn() -> BionicMallinfo> {
    static FOUND: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();
    let addr = (*FOUND.get_or_init(|| host_symbol(c"mallinfo2")))?;
    // SAFETY: `mallinfo2` takes no arguments and returns the ten-`size_t`
    // struct `BionicMallinfo` mirrors exactly.
    Some(unsafe { std::mem::transmute::<usize, extern "C" fn() -> BionicMallinfo>(addr) })
}

/// The address of `name` in the already-loaded host libc, or `None` if the
/// host has no such symbol.
fn host_symbol(name: &CStr) -> Option<usize> {
    // SAFETY: a plain symbol lookup with the default handle and a C string the
    // borrow guarantees is NUL-terminated.
    let p = unsafe { libc_dlsym(std::ptr::null_mut(), name.as_ptr()) };
    (!p.is_null()).then_some(p as usize)
}

impl BionicMallinfo {
    /// What a host without `mallinfo2` reports: all zeroes, never a guess.
    const ZERO: BionicMallinfo = BionicMallinfo {
        arena: 0, ordblks: 0, smblks: 0, hblks: 0, hblkhd: 0,
        usmblks: 0, fsmblks: 0, uordblks: 0, fordblks: 0, keepcost: 0,
    };
}

/// **Reports the host allocator's arena, which may not be the engine's.**
///
/// `mimalloc_lib` exists, so whether these numbers describe the heap Roblox is
/// actually allocating from depends on what ends up overriding `malloc` in a
/// given run. They are the truth about glibc's arena either way, and that is
/// what the name promises; nothing here invents a number. `INFERRED` that the
/// engine only uses this for telemetry -- no behaviour change was observed when
/// it went from a stub to this.
unsafe extern "C" fn bionic_mallinfo() -> BionicMallinfo {
    host_mallinfo2().map_or(BionicMallinfo::ZERO, |f| f())
}

/// `__cxa_thread_atexit_impl` — **not a divergence, simply absent.**
///
/// Issue #14 lists this beside `mallinfo` as an ABI divergence and it is not
/// one: bionic and glibc declare the identical
/// `int (*)(void (*)(void*), void*, void*)`, so the only problem was that
/// `libc` is not host-resolved by default and this was therefore reaching a
/// stub. A stub here means every `thread_local` with a destructor in the
/// engine never runs it -- a leak per thread, silent, and not a crash, which
/// is why nothing pointed at it.
///
/// Forwarded rather than reimplemented. glibc's own implementation is what
/// registers the callback with the thread's exit path, and there is no second
/// place to put it.
unsafe extern "C" fn bionic_cxa_thread_atexit_impl(
    func: *mut c_void,
    arg: *mut c_void,
    dso_handle: *mut c_void,
) -> c_int {
    // SAFETY: the three pointers are passed straight through to the host
    // implementation of the same function, with the same signature.
    unsafe { host_cxa_thread_atexit_impl(func, arg, dso_handle) }
}

unsafe extern "C" fn bionic_errno() -> *mut c_int {
    // SAFETY: glibc's per-thread errno slot; the same contract as bionic's.
    unsafe { __errno_location() }
}

/// Set this thread's `errno`.
fn set_errno(value: c_int) {
    // SAFETY: glibc's per-thread errno slot, valid for this thread's lifetime.
    unsafe { *__errno_location() = value };
}

/// An argument string for a diagnostic.
///
/// # Safety
/// `p` is either null or a NUL-terminated C string.
unsafe fn cstr(p: *const c_char) -> String {
    // SAFETY: the contract is the caller's; the helper checks for null.
    lossy_or_null(unsafe { borrow_request(p) })
}

/// What `__assert` prints, as one block, before the process is stopped.
fn assert_report(file: &str, line: c_int, msg: &str) -> String {
    format!("\n*** Roblox assertion failed ***\n    {file}:{line}: {msg}")
}

/// What `__assert2` prints: the same banner, with the function and the failed
/// expression on their own lines.
fn assert2_report(file: &str, line: c_int, function: &str, failed: &str) -> String {
    format!(
        "\n*** Roblox assertion failed ***\n    {file}:{line} in {function}\n    assertion: {failed}"
    )
}

/// Say why, show which stubs were hit on the way (a failed assertion with
/// corrupt state behind it is usually downstream of one), and stop.
fn die_with(report: &str) -> ! {
    eprintln!("{report}");
    crate::stubs::report();
    std::process::abort();
}

unsafe extern "C" fn bionic_assert(file: *const c_char, line: c_int, msg: *const c_char) -> ! {
    // SAFETY: bionic passes NUL-terminated strings or null.
    let (file, msg) = unsafe { (cstr(file), cstr(msg)) };
    die_with(&assert_report(&file, line, &msg))
}

unsafe extern "C" fn bionic_assert2(
    file: *const c_char,
    line: c_int,
    function: *const c_char,
    failed: *const c_char,
) -> ! {
    // SAFETY: bionic passes NUL-terminated strings or null.
    let (file, function, failed) = unsafe { (cstr(file), cstr(function), cstr(failed)) };
    die_with(&assert2_report(&file, line, &function, &failed))
}

/// bionic's GNU-flavoured `strerror_r`, which returns `char*` rather than int.
///
/// The host's `strerror_r` is that same function, so its answer is returned
/// as it stands. This used to ignore the result and hand back `buf`, which
/// glibc leaves unwritten for a known errno (it returns a pointer to a static
/// string instead), so the engine read an uninitialised buffer.
unsafe extern "C" fn gnu_strerror_r(errnum: c_int, buf: *mut c_char, buflen: usize) -> *mut c_char {
    // SAFETY: caller-supplied buffer of at least `buflen` bytes.
    unsafe { glibc_gnu_strerror_r(errnum, buf, buflen) }
}

/// bionic's plain `strerror_r`, which is POSIX's: fills `buf`, returns 0, or
/// an error number (`EINVAL` for an unknown errno, `ERANGE` if truncated).
///
/// Left to the host this resolved to glibc's GNU `strerror_r`, which returns
/// a pointer. libc++'s `system_category().message()` reads that return as an
/// int, sees the low half of an address, and calls `abort()` while the
/// `std::system_error` is still being built, so a `catch` never sees it.
/// INFERRED for a caught exception: the abort was read off the 2.738.1397
/// arm64 and x86-64 builds' `message()`, and observed only through a
/// coredump of an uncaught one.
unsafe extern "C" fn bionic_strerror_r(errnum: c_int, buf: *mut c_char, buflen: usize) -> c_int {
    // SAFETY: caller-supplied buffer of at least `buflen` bytes.
    unsafe { __xpg_strerror_r(errnum, buf.cast(), buflen) }
}

// The FORTIFY wrappers below are forwarded as they are, on purpose. Each is its
// unchecked counterpart minus a bound, and the contract that matters is the
// host function's own: a null `s` is as much a fault in `strlen` here as it
// was on Android. Turning that into a quiet 0 or an `EINVAL` would make a
// caller's bug look like an answer, which is the kind of lie a stub must not
// tell, so they are validated by the host and not re-validated here.

unsafe extern "C" fn strlen_chk(s: *const c_char, _bound: usize) -> usize {
    // SAFETY: the caller's NUL-terminated string, forwarded to the host.
    unsafe { strlen(s) }
}

unsafe extern "C" fn strchr_chk(s: *const c_char, c: c_int, _bound: usize) -> *mut c_char {
    // SAFETY: the caller's NUL-terminated string, forwarded to the host.
    unsafe { strchr(s, c) }
}

/// Absent until Roblox 2.702.0.622, whose `libroblox.so` imports it: glibc has
/// no `__strrchr_chk`, so the load stopped at "cannot locate symbol" before a
/// single engine instruction ran. Same contract as `__strchr_chk`.
unsafe extern "C" fn strrchr_chk(s: *const c_char, c: c_int, _bound: usize) -> *mut c_char {
    // SAFETY: the caller's NUL-terminated string, forwarded to the host.
    unsafe { strrchr(s, c) }
}

unsafe extern "C" fn strncpy_chk2(
    dst: *mut c_char,
    src: *const c_char,
    n: usize,
    _dst_len: usize,
    _src_len: usize,
) -> *mut c_char {
    // SAFETY: the caller's buffers, forwarded with the caller's own count.
    unsafe { strncpy(dst, src, n) }
}

unsafe extern "C" fn write_chk(fd: c_int, buf: *const c_void, count: usize, _bound: usize) -> isize {
    // SAFETY: the caller's buffer and count; the kernel reports a bad one as EFAULT.
    unsafe { write(fd, buf, count) }
}

unsafe extern "C" fn umask_chk(mode: u32) -> u32 {
    // SAFETY: a plain scalar.
    unsafe { umask(mode) }
}

unsafe extern "C" fn poll_chk(fds: *mut c_void, nfds: u64, timeout: c_int, _bound: usize) -> c_int {
    // SAFETY: the caller's `struct pollfd` array, identical in both libcs.
    unsafe { poll(fds, nfds, timeout) }
}

unsafe extern "C" fn readlink_chk(
    path: *const c_char,
    buf: *mut c_char,
    size: usize,
    _bound: usize,
) -> isize {
    // SAFETY: the caller's path and buffer, forwarded with the caller's own size.
    unsafe { readlink(path, buf, size) }
}

unsafe extern "C" fn fread_chk(
    ptr: *mut c_void,
    size: usize,
    n: usize,
    stream: *mut c_void,
    _bound: usize,
) -> usize {
    // SAFETY: the caller's buffer and `FILE*`, forwarded with the caller's own counts.
    unsafe { fread(ptr, size, n, stream) }
}

/// What Cordial reports for `ro.*` system properties.
///
/// These are the values §4.2's "Roblox thinks you're mobile" fix turns on. They
/// are guesses until the client is observed reacting to them; the point for now
/// is that the call succeeds and returns something desktop-shaped rather than an
/// empty string.
const PROPERTIES: &[(&str, &str)] = &[
    // API 33 / Android 13, which is what the Waydroid capture reports the real
    // client running as: `OS Ver. = 13, Lvl = 33`. Those are two different
    // numbers and the engine wants both — the release string and the API level.
    // Reporting a level the engine considers old costs real capability; at 15 it
    // refused Vulkan outright ("Android version is too old to activate Vulkan").
    ("ro.build.version.sdk", "33"),
    ("ro.build.version.release", "13"),
    ("ro.product.model", "Cordial"),
    ("ro.product.manufacturer", "Cordial"),
    ("ro.product.brand", "cordial"),
    ("ro.product.device", "linux"),
    ("ro.product.name", "cordial"),
    ("ro.hardware", "cordial"),
    // **`ro.soc.manufacturer` is deliberately not here, and this comment is the
    // answer to issue #12 rather than a note that nobody got round to it.**
    //
    // The engine asks for it twice a run and gets `""`, which is what an unset
    // Android property returns -- plenty of real devices leave it unset, since
    // it only arrived in API 31. So the empty answer is not a gap in the table;
    // it is a value Android itself produces.
    //
    // The issue asked to establish what the field is used for before filling
    // it. That was checked the way this repository checks such things first:
    // `docs/traces/waydroid-roblox-startup.log.gz`, a capture of the same APK
    // on real Android, does not mention `ro.soc` anywhere -- so there is no
    // evidence the engine does anything with the answer, and none that a
    // different one would change behaviour.
    //
    // Filling it in would mean choosing a SoC vendor. Cordial is not running on
    // one, every plausible string is a lie the engine may act on, and AGENTS.md
    // is explicit that a stub which lies is worse than one which fails. `""`
    // fails honestly, so `""` stays. If something is ever traced to it, this is
    // the comment to come back to and the trace is the thing that would settle
    // it -- not a guess at a vendor name.
    ("ro.board.platform", "cordial"),
    ("ro.debuggable", "0"),
    ("ro.secure", "1"),
];

/// bionic's `android_get_device_api_level()`.
///
/// Must agree with `ro.build.version.sdk`: code that reads both and finds them
/// inconsistent is code that will believe the smaller one.
unsafe extern "C" fn android_get_device_api_level() -> c_int {
    33
}

/// bionic's `__system_property_get(name, value)`. `value` is a caller buffer of
/// `PROP_VALUE_MAX` (92) bytes; the return is the length written.
unsafe extern "C" fn system_property_get(name: *const c_char, value: *mut c_char) -> c_int {
    if value.is_null() {
        return 0;
    }
    // SAFETY: bionic's contract is a NUL-terminated name and a 92-byte buffer.
    let key = unsafe { cstr(name) };
    let found = property_value(&key);

    // `CORDIAL_TRACE_PROPS=1` names every property the engine asks for and what
    // it was told.
    //
    // An unknown key returns the empty string, and an empty string handed to
    // something that then builds a path out of it is indistinguishable from a
    // path that was never configured. `RbxStorage::init` fails during ELF
    // construction on exactly that shape -- three `stat("")` in a row -- and
    // system properties are one of the few things readable that early on
    // Android and absent here. See docs/analysis/flag-init.md §26.
    if std::env::var_os("CORDIAL_TRACE_PROPS").is_some() {
        eprintln!(
            "[props] {key} = {}",
            if found.is_empty() { "<empty, not in table>" } else { found }
        );
    }

    // The reply is at most PROP_VALUE_MAX bytes, NUL included, so the helper's
    // own length check is satisfied by the buffer the ABI promises. It writes
    // the reply and its NUL and nothing past them, as the hand-rolled copy did.
    let reply = property_reply(found);
    if !write_response(&reply, value, PROP_VALUE_MAX) {
        return 0;
    }
    (reply.len() - 1) as c_int
}

/// `PROP_VALUE_MAX`: the size of the buffer bionic's callers pass, NUL included.
const PROP_VALUE_MAX: usize = 92;

/// The value Cordial reports for `key`, or the empty string every unset
/// Android property reads as.
fn property_value(key: &str) -> &'static str {
    PROPERTIES
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| *v)
        .unwrap_or("")
}

/// `value` as `__system_property_get` leaves it in the caller's buffer:
/// truncated to `PROP_VALUE_MAX - 1` bytes and NUL-terminated.
fn property_reply(value: &str) -> Vec<u8> {
    let bytes = value.as_bytes();
    let mut reply = bytes[..bytes.len().min(PROP_VALUE_MAX - 1)].to_vec();
    reply.push(0);
    reply
}

// ---------------------------------------------------------------------- sysconf

pub mod sysconf_table;

unsafe extern "C" {
    fn sysconf(name: c_int) -> i64;
}

/// bionic's `sysconf`, translated.
///
/// The selectors are not shared: bionic's `_SC_PAGESIZE` is 39, glibc's is 30,
/// and glibc reads 39 as `_SC_BC_STRING_MAX`. Roblox asks for the page size
/// during static initialisation and, untranslated, is told 1000 — which is not a
/// power of two, so the allocator that asked aborts. Nothing about that failure
/// points back at `sysconf`, which is why the translation is worth its table.
unsafe extern "C" fn bionic_sysconf(name: c_int) -> i64 {
    match classify_sysconf(name) {
        // SAFETY: `glibc` came from the generated table and is a valid selector.
        Sysconf::Host(glibc) => unsafe { sysconf(glibc) },
        Sysconf::Unsupported(sym) => {
            eprintln!("[bionic] sysconf({sym}) has no glibc equivalent; returning -1");
            -1
        }
        Sysconf::Unknown => {
            eprintln!("[bionic] sysconf({name}) is not a selector bionic defines; returning -1");
            -1
        }
    }
}

/// What a bionic `sysconf` selector means on this host.
#[derive(Debug, PartialEq, Eq)]
enum Sysconf {
    /// Ask the host, with this glibc selector.
    Host(c_int),
    /// A bionic selector glibc has nothing for, by its `_SC_*` name.
    Unsupported(&'static str),
    /// Not a selector bionic defines at all.
    Unknown,
}

fn classify_sysconf(name: c_int) -> Sysconf {
    if let Some(&(_, glibc, _)) = sysconf_table::SYSCONF_MAP
        .iter()
        .find(|(bionic, _, _)| *bionic == name)
    {
        return Sysconf::Host(glibc);
    }
    match sysconf_table::UNSUPPORTED.iter().find(|(bionic, _)| *bionic == name) {
        Some(&(_, sym)) => Sysconf::Unsupported(sym),
        None => Sysconf::Unknown,
    }
}

// --------------------------------------------------------------------- pathconf

unsafe extern "C" {
    #[link_name = "pathconf"]
    fn host_pathconf(path: *const c_char, name: c_int) -> i64;
}

/// `(bionic _PC_* value, glibc value)`. The same problem as `sysconf` on a
/// smaller scale: bionic numbers its twenty selectors in its own order,
/// starting with `_PC_FILESIZEBITS` at 0, while glibc starts with
/// `_PC_LINK_MAX` and puts `_PC_FILESIZEBITS` at 13. Not one of the twenty
/// agrees, so forwarding untranslated would answer `_PC_NAME_MAX` when asked
/// for `_PC_PATH_MAX`. Bionic's values are from
/// `third_party/mcpelauncher-linker/bionic/libc/include/unistd.h`, glibc's from
/// `bits/confname.h`; bionic has no `_PC_SOCK_MAXBUF`, so glibc's 12 is never a
/// target.
const PATHCONF_MAP: &[(c_int, c_int)] = &[
    (0, 13),  // _PC_FILESIZEBITS
    (1, 0),   // _PC_LINK_MAX
    (2, 1),   // _PC_MAX_CANON
    (3, 2),   // _PC_MAX_INPUT
    (4, 3),   // _PC_NAME_MAX
    (5, 4),   // _PC_PATH_MAX
    (6, 5),   // _PC_PIPE_BUF
    (7, 20),  // _PC_2_SYMLINKS
    (8, 18),  // _PC_ALLOC_SIZE_MIN
    (9, 14),  // _PC_REC_INCR_XFER_SIZE
    (10, 15), // _PC_REC_MAX_XFER_SIZE
    (11, 16), // _PC_REC_MIN_XFER_SIZE
    (12, 17), // _PC_REC_XFER_ALIGN
    (13, 19), // _PC_SYMLINK_MAX
    (14, 6),  // _PC_CHOWN_RESTRICTED
    (15, 7),  // _PC_NO_TRUNC
    (16, 8),  // _PC_VDISABLE
    (17, 10), // _PC_ASYNC_IO
    (18, 11), // _PC_PRIO_IO
    (19, 9),  // _PC_SYNC_IO
];

/// bionic's `pathconf`, translated.
///
/// The Quest build's OpenXR loader imports it and nothing else here did, so
/// until now it was unresolvable without `--host-libc` -- and with it, the host
/// would have been handed bionic's selector numbers and answered a different
/// question without complaint. The path is passed through untouched: a caller
/// asking about `/system` would want `system_paths.cpp`'s redirection, but
/// nothing has been seen to, and redirecting on a guess is worse than not.
unsafe extern "C" fn bionic_pathconf(path: *const c_char, name: c_int) -> i64 {
    match pathconf_selector(name) {
        // SAFETY: `path` is the caller's C string, forwarded as-is to the host
        // function with the same signature; `glibc` is a valid selector.
        Some(glibc) => unsafe { host_pathconf(path, glibc) },
        None => {
            // What bionic itself does for a selector it does not know.
            set_errno(22); // EINVAL
            -1
        }
    }
}

/// glibc's `_PC_*` for a bionic one, or `None` if bionic defines no such selector.
fn pathconf_selector(bionic: c_int) -> Option<c_int> {
    PATHCONF_MAP.iter().find(|(b, _)| *b == bionic).map(|&(_, glibc)| glibc)
}

/// bionic's `isxdigit`. The ASCII answer, which is bionic's whatever the
/// locale; glibc's reads the current locale's table, and nothing is gained by
/// letting the host's locale decide what a hex digit is for the guest.
unsafe extern "C" fn bionic_isxdigit(c: c_int) -> c_int {
    u8::try_from(c).is_ok_and(|b| b.is_ascii_hexdigit()) as c_int
}

#[cfg(test)]
mod fortify_tests {
    use super::*;

    #[test]
    fn strrchr_chk_is_provided_and_finds_the_last_match() {
        let overrides = function_overrides();
        assert!(
            overrides.iter().any(|(name, _)| *name == "__strrchr_chk"),
            "2.702.0.622 imports __strrchr_chk and glibc does not define it"
        );
        let path = c"/a/b/c.txt";
        let got = strrchr_chk(path.as_ptr(), b'/' as c_int, path.to_bytes_with_nul().len());
        // SAFETY: `got` points into `path`.
        assert_eq!(unsafe { CStr::from_ptr(got) }, c"/c.txt");
        assert!(strrchr_chk(path.as_ptr(), b'z' as c_int, 11).is_null());
    }
}

#[cfg(test)]
mod inner_tests {
    use super::*;

    #[test]
    fn the_assertion_reports_name_file_line_and_text() {
        assert_eq!(
            assert_report("a.cpp", 12, "x > 0"),
            "\n*** Roblox assertion failed ***\n    a.cpp:12: x > 0"
        );
        assert_eq!(
            assert2_report("a.cpp", 12, "run", "x > 0"),
            "\n*** Roblox assertion failed ***\n    a.cpp:12 in run\n    assertion: x > 0"
        );
        // A null argument prints as such rather than faulting while the process
        // is already on its way out.
        // SAFETY: null is explicitly allowed by `cstr`.
        let null = unsafe { cstr(std::ptr::null()) };
        assert_eq!(assert_report(&null, 0, &null), "\n*** Roblox assertion failed ***\n    (null):0: (null)");
        // SAFETY: a literal is NUL-terminated.
        assert_eq!(unsafe { cstr(c"file.cpp".as_ptr()) }, "file.cpp");
    }

    #[test]
    fn a_property_reply_is_truncated_to_fit_the_buffer_and_always_terminated() {
        assert_eq!(property_reply("33"), b"33\0");
        assert_eq!(property_reply(""), b"\0");
        let long = "x".repeat(300);
        let reply = property_reply(&long);
        assert_eq!(reply.len(), PROP_VALUE_MAX, "91 bytes and the NUL");
        assert_eq!(reply.last(), Some(&0));
        assert!(reply[..PROP_VALUE_MAX - 1].iter().all(|&b| b == b'x'));
    }

    #[test]
    fn every_reported_property_fits_a_prop_value_max_buffer_untruncated() {
        for (key, value) in PROPERTIES {
            assert!(value.len() < PROP_VALUE_MAX, "{key} would be cut short");
            assert_eq!(property_value(key), *value);
        }
        assert_eq!(property_value("ro.soc.manufacturer"), "", "deliberately absent, see the table");
        assert_eq!(property_value("no.such.property"), "");
    }

    #[test]
    fn property_get_writes_the_value_and_its_nul_and_not_a_byte_more() {
        let mut buf = [0xAAu8; 100];
        let n = system_property_get(c"ro.build.version.sdk".as_ptr(), buf.as_mut_ptr() as *mut c_char);
        assert_eq!(n, 2);
        assert_eq!(&buf[..4], b"33\0\xAA", "the byte after the NUL is the caller's");

        // An unknown name is the empty string, as an unset Android property is.
        let mut unknown = [0xAAu8; 8];
        assert_eq!(system_property_get(c"nope".as_ptr(), unknown.as_mut_ptr() as *mut c_char), 0);
        assert_eq!(&unknown[..2], b"\0\xAA");

        // And a null name is looked up as the string "(null)", which is not set.
        let mut null_name = [0xAAu8; 8];
        assert_eq!(system_property_get(std::ptr::null(), null_name.as_mut_ptr() as *mut c_char), 0);
        assert_eq!(null_name[0], 0);
    }

    #[test]
    fn a_property_with_nowhere_to_go_is_zero_and_touches_nothing() {
        assert_eq!(system_property_get(c"ro.build.version.sdk".as_ptr(), std::ptr::null_mut()), 0);
    }

    #[test]
    fn a_sysconf_selector_is_classified_by_the_table_it_came_from() {
        // 39 is bionic's _SC_PAGESIZE and glibc's _SC_BC_STRING_MAX is also 39:
        // the case the translation exists for.
        assert_eq!(classify_sysconf(39), Sysconf::Host(30));
        assert_eq!(classify_sysconf(99_999), Sysconf::Unknown);
        for &(bionic, _) in sysconf_table::UNSUPPORTED {
            assert!(
                matches!(classify_sysconf(bionic), Sysconf::Unsupported(_)),
                "{bionic} is in the unsupported list"
            );
        }
        // The two lists never overlap, or the mapped one would win silently
        // over a selector the generator also marked unsupported.
        for &(bionic, _) in sysconf_table::UNSUPPORTED {
            assert!(
                !sysconf_table::SYSCONF_MAP.iter().any(|(b, _, _)| *b == bionic),
                "{bionic} is both mapped and unsupported"
            );
        }
    }

    #[test]
    fn sysconf_answers_the_page_size_in_the_hosts_terms() {
        // SAFETY: glibc's own selector, 30 = _SC_PAGESIZE.
        let host = unsafe { sysconf(30) };
        assert!(host > 0 && (host & (host - 1)) == 0, "a power of two, or the allocator aborts");
        assert_eq!(bionic_sysconf(39), host);
        assert_eq!(bionic_sysconf(99_999), -1);
    }

    #[test]
    fn pathconf_has_twenty_distinct_selectors_and_no_others() {
        let mut targets: Vec<c_int> = (0..20).map(|b| pathconf_selector(b).expect("defined")).collect();
        targets.sort_unstable();
        targets.dedup();
        assert_eq!(targets.len(), 20, "no two bionic selectors may share a glibc one");
        assert_eq!(pathconf_selector(20), None);
        assert_eq!(pathconf_selector(-1), None);
    }

    #[test]
    fn mallinfo_reports_the_host_arena_or_nothing_and_never_garbage() {
        let _keep_the_heap_non_empty = vec![0u8; 4096];
        let info = bionic_mallinfo();
        if host_mallinfo2().is_some() {
            assert!(info.uordblks > 0, "a running test process has allocated memory");
        } else {
            assert_eq!(info.uordblks, 0);
        }
        assert_eq!(BionicMallinfo::ZERO.arena, 0);
    }

    #[test]
    fn a_host_symbol_is_found_or_reported_missing() {
        assert!(host_symbol(c"getpid").is_some());
        assert!(host_symbol(c"cordial_no_such_symbol_anywhere").is_none());
    }

    #[test]
    fn the_fortify_wrappers_forward_to_the_host_unchanged() {
        let s = c"hello/world";
        assert_eq!(strlen_chk(s.as_ptr(), 12), 11);
        // SAFETY: both results point into `s`.
        unsafe {
            assert_eq!(CStr::from_ptr(strchr_chk(s.as_ptr(), b'/' as c_int, 12)), c"/world");
        }
        let mut dst = [0x7f as c_char; 8];
        let out = strncpy_chk2(dst.as_mut_ptr(), c"abc".as_ptr(), 8, 8, 4);
        assert_eq!(out, dst.as_mut_ptr());
        assert_eq!(dst[..4], [b'a' as c_char, b'b' as c_char, b'c' as c_char, 0]);

        let mut link = [0 as c_char; 4096];
        let n = readlink_chk(c"/proc/self/exe".as_ptr(), link.as_mut_ptr(), link.len(), link.len());
        assert!(n > 0, "readlink of our own executable");
        assert_eq!(readlink_chk(c"/".as_ptr(), link.as_mut_ptr(), link.len(), link.len()), -1);
    }

    // ----------------------------------------------------- native symbol tables

    unsafe extern "C" fn fake_table(count: *mut usize) -> *const Symbol {
        struct Table([Symbol; 3]);
        // SAFETY: the fixture holds pointers to string literals and to nothing
        // that is ever written through, so sharing it is sound.
        unsafe impl Sync for Table {}
        static TABLE: Table = Table([
            Symbol { name: c"first".as_ptr(), addr: 0x10 as *mut c_void },
            Symbol { name: c"second".as_ptr(), addr: 0x20 as *mut c_void },
            // Not UTF-8: the name reads as empty rather than stopping the load.
            Symbol { name: b"\xff\xfe\0".as_ptr() as *const c_char, addr: 0x30 as *mut c_void },
        ]);
        // SAFETY: the real accessors do exactly this with their own count.
        unsafe { *count = TABLE.0.len() };
        TABLE.0.as_ptr()
    }

    unsafe extern "C" fn null_table(count: *mut usize) -> *const Symbol {
        // SAFETY: as above. A native side with nothing to say reports zero.
        unsafe { *count = 0 };
        std::ptr::null()
    }

    #[test]
    fn a_native_table_becomes_name_and_address_pairs() {
        let got = symbol_table(fake_table);
        assert_eq!(
            got,
            vec![
                ("first", 0x10 as *mut c_void),
                ("second", 0x20 as *mut c_void),
                ("", 0x30 as *mut c_void),
            ]
        );
    }

    #[test]
    fn a_null_native_table_is_an_empty_one() {
        assert!(symbol_table(null_table).is_empty());
    }

    #[test]
    fn the_real_native_tables_are_all_non_empty_and_named() {
        for (what, table) in [
            ("liblog", liblog_overrides()),
            ("system_paths", system_path_overrides()),
            ("thread_trace", thread_overrides()),
            ("netdb", netdb_overrides()),
            ("foreign_alloc", alloc_overrides()),
        ] {
            assert!(!table.is_empty(), "{what} registered nothing");
            assert!(table.iter().all(|(n, a)| !n.is_empty() && !a.is_null()), "{what}");
        }
    }
}

#[cfg(test)]
mod pathconf_tests {
    use super::*;

    #[test]
    fn bionic_path_max_is_the_hosts_path_max_not_its_name_max() {
        // 5 is bionic's _PC_PATH_MAX and glibc's _PC_PIPE_BUF; 4096 against
        // 255 is the difference the table exists for.
        let root = c"/".as_ptr();
        // SAFETY: a literal path and glibc's own selectors.
        let (path_max, name_max) = unsafe { (host_pathconf(root, 4), host_pathconf(root, 3)) };
        assert_eq!(bionic_pathconf(root, 5), path_max);
        assert_eq!(bionic_pathconf(root, 4), name_max);
        assert_ne!(path_max, name_max);
    }

    #[test]
    fn an_unknown_selector_fails_with_einval() {
        assert_eq!(bionic_pathconf(c"/".as_ptr(), 99), -1);
        // SAFETY: this thread's errno slot.
        assert_eq!(unsafe { *__errno_location() }, 22);
    }

    #[test]
    fn isxdigit_is_ascii_only() {
        for (c, want) in [(b'0', 1), (b'9', 1), (b'a', 1), (b'F', 1), (b'g', 0), (b' ', 0)] {
            assert_eq!(bionic_isxdigit(c as c_int), want, "{:?}", c as char);
        }
        assert_eq!(bionic_isxdigit(-1), 0);
        assert_eq!(bionic_isxdigit(0x1_30), 0);
    }
}

// ------------------------------------------------------------- native tables

/// One entry of a symbol table exported by `native/*.cpp`.
#[repr(C)]
struct Symbol {
    name: *const c_char,
    addr: *mut c_void,
}

/// The native side's accessor for its table: the entry count goes out through
/// the argument and the first entry is returned.
type SymbolFetch = unsafe extern "C" fn(count: *mut usize) -> *const Symbol;

/// Read a table of `(name, address)` pairs a C++ translation unit exports.
///
/// Seven copies of this lived inline, one per table, each with its own pair of
/// raw-pointer reads. A null table is an empty one, as every copy treated it,
/// and a name that is not UTF-8 reads as `""` rather than stopping the load.
fn symbol_table(fetch: SymbolFetch) -> Vec<(&'static str, *mut c_void)> {
    let mut count = 0usize;
    // SAFETY: each table is a static in its `.cpp` and outlives the process.
    let table = unsafe { fetch(&mut count) };
    if table.is_null() {
        return Vec::new();
    }
    // SAFETY: `table` points at `count` initialised entries with static names.
    let entries = unsafe { std::slice::from_raw_parts(table, count) };
    entries
        .iter()
        .map(|e| {
            // SAFETY: each `name` is a string literal in the `.cpp`.
            let name = unsafe { CStr::from_ptr(e.name) }.to_str().unwrap_or("");
            (name, e.addr)
        })
        .collect()
}

// ----------------------------------------------------------------------- liblog

/// Android's `liblog`, implemented in `native/liblog.cpp`.
///
/// Roblox narrates its own startup through these, so wiring them up is what turns
/// every later failure from silent into explained. Implemented in C++ because
/// three of the six are variadic.
pub fn liblog_overrides() -> Vec<(&'static str, *mut c_void)> {
    unsafe extern "C" {
        fn cordial_liblog_symbols(count: *mut usize) -> *const Symbol;
    }
    symbol_table(cordial_liblog_symbols)
}

// -------------------------------------------------------------------- OpenSL

/// OpenSL ES, implemented in `native/opensles.cpp` and backed by PipeWire via
/// `native/pipewire_backend.cpp`.
///
/// Seven of these eight are *data* symbols (`SLInterfaceID` is a pointer to a
/// UUID struct), and a missing data symbol fails the `DT_NEEDED` walk outright
/// rather than at first use. Current Roblox builds reference them directly,
/// which is why `libOpenSLES.so` can no longer be an empty library.
///
/// The object model (engine, output mix, buffer-queue-sourced audio players)
/// is implemented and plays through PipeWire when it is reachable. When it is
/// not — no library, no session, or built without `pipewire-devel` — the same
/// `slCreateEngine` reports failure rather than pretending, only for a more
/// specific reason than "nothing is implemented yet". See `opensles.cpp` and
/// `pipewire_backend.cpp` for the split.
pub fn opensles_overrides() -> Vec<(&'static str, *mut c_void)> {
    unsafe extern "C" {
        fn cordial_opensles_symbols(count: *mut usize) -> *const Symbol;
    }
    symbol_table(cordial_opensles_symbols)
}

// -------------------------------------------------------------------- AAudio

/// Whether `CORDIAL_AUDIO` selected an AAudio mode. True for an unset
/// variable: AAudio is the default.
///
/// Read from `native/aaudio.cpp` rather than from `std::env` here so that
/// there is exactly one parser of the variable in the process. The failure
/// this forecloses is specific: `symtab` deciding to register
/// `libaaudio.so` while `org.fmod.FMOD.supportsAAudio()` in
/// `native/audio_classes.cpp` says false, which would leave FMOD on the Java
/// path with a virtual library nobody opens — an experiment that looks like
/// it ran and did not.
pub fn aaudio_selected() -> bool {
    unsafe extern "C" {
        fn cordial_audio_backend_is_aaudio() -> c_int;
    }
    // SAFETY: reads a process-wide `static` decided once in aaudio.cpp.
    unsafe { cordial_audio_backend_is_aaudio() != 0 }
}

/// Prints which audio backend is in force, during startup.
///
/// `CORDIAL_AUDIO` was described in conversation as the intended design and
/// then tried on a live run before it existed, where it was silently ignored.
/// That is indistinguishable from a feature that did nothing, so the switch
/// announces itself rather than leaving the reader to infer it from whether
/// sound comes out.
pub fn announce_audio_backend() {
    unsafe extern "C" {
        fn cordial_audio_backend_announce();
    }
    // SAFETY: no arguments, no return; writes one line to stderr.
    unsafe { cordial_audio_backend_announce() }
}

/// AAudio, implemented in `native/aaudio.cpp` over
/// `native/pipewire_backend.cpp`'s `CallbackStream`.
///
/// Unlike OpenSL ES above, none of these are linked: `libroblox.so` has no
/// undefined `AAudio*` symbols at all. It reaches them by `dlopen`ing
/// `libaaudio.so` and `dlsym`ing the 25 names in
/// `docs/analysis/aaudio-contract.md` — but only after
/// `org.fmod.FMOD.supportsAAudio()` has said yes, which is the actual gate
/// and which was measured, not assumed; see the comment on that method.
///
/// So this is registered the way `android::vulkan` and `mimalloc_lib` are, as
/// a virtual library of its own — by default, since 2026-08-22, with
/// `CORDIAL_AUDIO=java` the way back to FMOD's Java `AudioDevice` path.
pub fn aaudio_overrides() -> Vec<(&'static str, *mut c_void)> {
    unsafe extern "C" {
        fn cordial_aaudio_symbols(count: *mut usize) -> *const Symbol;
    }
    symbol_table(cordial_aaudio_symbols)
}

#[cfg(test)]
mod aaudio_tests {
    /// The 25 names `libroblox.so` 2.734.0.917 looks up, from
    /// `docs/analysis/aaudio-contract.md`.
    ///
    /// Duplicated here on purpose rather than generated from the C++ table:
    /// this is the assertion that the table and the measurement still agree.
    /// A typo in one entry of `aaudio.cpp`'s `kSymbols` is otherwise silent —
    /// the guest's `dlsym` returns null for that one name, FMOD carries on
    /// with a null function pointer, and what surfaces is a crash or a mute
    /// stream a long way from the cause.
    const MEASURED: &[&str] = &[
        "AAudio_createStreamBuilder",
        "AAudioStreamBuilder_delete",
        "AAudioStreamBuilder_openStream",
        "AAudioStreamBuilder_setBufferCapacityInFrames",
        "AAudioStreamBuilder_setDataCallback",
        "AAudioStreamBuilder_setDirection",
        "AAudioStreamBuilder_setErrorCallback",
        "AAudioStreamBuilder_setFormat",
        "AAudioStreamBuilder_setInputPreset",
        "AAudioStreamBuilder_setPerformanceMode",
        "AAudioStreamBuilder_setUsage",
        "AAudioStream_close",
        "AAudioStream_getBufferCapacityInFrames",
        "AAudioStream_getBufferSizeInFrames",
        "AAudioStream_getChannelCount",
        "AAudioStream_getFormat",
        "AAudioStream_getFramesPerBurst",
        "AAudioStream_getSampleRate",
        "AAudioStream_getState",
        "AAudioStream_getXRunCount",
        "AAudioStream_read",
        "AAudioStream_requestPause",
        "AAudioStream_requestStart",
        "AAudioStream_requestStop",
        "AAudioStream_setBufferSizeInFrames",
    ];

    #[test]
    fn exports_exactly_the_symbols_the_engine_looks_up() {
        let mut got: Vec<&str> = super::aaudio_overrides().into_iter().map(|(n, _)| n).collect();
        got.sort_unstable();
        let mut want: Vec<&str> = MEASURED.to_vec();
        want.sort_unstable();
        assert_eq!(got, want);
    }

    #[test]
    fn every_export_has_a_real_address() {
        // A null here would register the name and resolve it to nothing,
        // which is worse than not registering it at all: the guest's `dlsym`
        // succeeds and the call goes to address zero.
        for (name, addr) in super::aaudio_overrides() {
            assert!(!addr.is_null(), "{name} resolved to a null address");
        }
    }
}

// ------------------------------------------------------------------ /system

/// The path-taking libc calls, redirected for `/system` — `native/system_paths.cpp`.
///
/// Roblox asks for `/system/fonts/NotoSansCJK-Regular.ttc`, and on a host with
/// no `/system` the failed lookup becomes an empty path and an unhandled
/// `Path does not exist: ""` during app startup. `open` is variadic, which is
/// why this lives in C++ alongside liblog rather than in `trace.rs`.
pub fn system_path_overrides() -> Vec<(&'static str, *mut c_void)> {
    unsafe extern "C" {
        fn cordial_system_symbols(count: *mut usize) -> *const Symbol;
    }
    symbol_table(cordial_system_symbols)
}

/// `pthread_create` — `native/thread_trace.cpp`.
///
/// Off by default and behaviourally identical to the unwrapped host function
/// either way; the flag only decides whether the new thread announces its
/// creator, its start routine and its own tid before running anything else.
/// See `docs/analysis/flag-init.md` §29 for why this exists: the thread that
/// runs `RbxStorage::init`'s failing `stat("")` calls was never traced back to
/// whoever spawned it.
pub fn thread_overrides() -> Vec<(&'static str, *mut c_void)> {
    unsafe extern "C" {
        fn cordial_thread_symbols(count: *mut usize) -> *const Symbol;
    }
    symbol_table(cordial_thread_symbols)
}

// ------------------------------------------------------------------- netdb

/// `getaddrinfo`/`freeaddrinfo` in bionic's layout — `native/netdb_compat.cpp`.
///
/// Without these the engine cannot resolve a single hostname: bionic's
/// `AI_DEFAULT` sets a bit glibc rejects with `EAI_BADFLAGS`, and a result that
/// did come back would have its `ai_addr` and `ai_canonname` transposed.
pub fn netdb_overrides() -> Vec<(&'static str, *mut c_void)> {
    unsafe extern "C" {
        fn cordial_netdb_symbols(count: *mut usize) -> *const Symbol;
    }
    symbol_table(cordial_netdb_symbols)
}

// ------------------------------------------------------------ foreign frees

/// `vasprintf` and `getcwd` refused in their allocating forms —
/// `native/foreign_alloc.cpp`.
///
/// The engine runs its own statically linked allocator and imports no `free`,
/// so a block glibc allocated cannot be released by it. `s_realpath` established
/// the pattern: never produce the host allocation. ADR-040 has the measurement.
pub fn alloc_overrides() -> Vec<(&'static str, *mut c_void)> {
    unsafe extern "C" {
        fn cordial_alloc_symbols(count: *mut usize) -> *const Symbol;
    }
    symbol_table(cordial_alloc_symbols)
}

#[cfg(test)]
mod foreign_alloc_tests {
    use super::*;

    unsafe extern "C" {
        #[link_name = "__errno_location"]
        fn libc_errno() -> *mut c_int;
    }

    fn find(name: &str) -> *mut c_void {
        function_overrides()
            .into_iter()
            .find(|(n, _)| *n == name)
            .unwrap_or_else(|| panic!("{name} is not overridden"))
            .1
    }

    /// The point of the whole change: the engine's `getcwd` import lands on the
    /// refusal, not on glibc. Reads the override table the loader consults, not
    /// the C++ symbol directly, so a table that lost the entry fails here.
    #[test]
    fn getcwd_null_buffer_is_refused_through_the_override_table() {
        type Getcwd = unsafe extern "C" fn(*mut c_char, usize) -> *mut c_char;
        // SAFETY: the address is `cordial_getcwd`, which has this signature.
        let getcwd: Getcwd = unsafe { std::mem::transmute(find("getcwd")) };

        // SAFETY: a null buffer is the case under test; the real one is 4096 bytes.
        unsafe {
            *libc_errno() = 0;
            assert!(getcwd(std::ptr::null_mut(), 0).is_null());
            assert_eq!(*libc_errno(), 12, "ENOMEM");

            let mut buf = [0 as c_char; 4096];
            let got = getcwd(buf.as_mut_ptr(), buf.len());
            assert_eq!(got, buf.as_mut_ptr());
            let want = std::env::current_dir().unwrap();
            assert_eq!(
                CStr::from_ptr(buf.as_ptr()).to_str().unwrap(),
                want.to_str().unwrap()
            );
        }
    }

    #[test]
    fn vasprintf_is_overridden() {
        assert!(!find("vasprintf").is_null());
    }
}

#[cfg(test)]
mod strerror_tests {
    use super::*;

    fn find(name: &str) -> *mut c_void {
        function_overrides()
            .into_iter()
            .find(|(n, _)| *n == name)
            .unwrap_or_else(|| panic!("{name} is not overridden"))
            .1
    }

    type Xsi = unsafe extern "C" fn(c_int, *mut c_char, usize) -> c_int;
    type Gnu = unsafe extern "C" fn(c_int, *mut c_char, usize) -> *mut c_char;

    /// The engine's int-returning import must land on the POSIX contract:
    /// zero, and the text in the caller's buffer. Reads the table the loader
    /// consults, so an entry that was dropped fails here rather than aborting
    /// the engine the first time a `std::system_error` is built.
    #[test]
    fn plain_strerror_r_is_the_posix_one() {
        // SAFETY: the address is `bionic_strerror_r`, which has this signature.
        let f: Xsi = unsafe { std::mem::transmute(find("strerror_r")) };
        let mut buf = [0 as c_char; 256];
        // SAFETY: `buf` is 256 bytes and the length passed says so.
        unsafe {
            assert_eq!(f(22, buf.as_mut_ptr(), buf.len()), 0);
            assert_eq!(CStr::from_ptr(buf.as_ptr()).to_str().unwrap(), "Invalid argument");

            // An errno glibc has no text for is EINVAL, and libc++ handles
            // exactly that answer by writing its own "Unknown error N".
            let mut unknown = [0 as c_char; 256];
            assert_eq!(f(99_999, unknown.as_mut_ptr(), unknown.len()), 22);
            assert!(CStr::from_ptr(unknown.as_ptr()).to_str().unwrap().contains("Unknown error"));
        }
    }

    /// The `char*` flavour must return the message, not a buffer glibc never
    /// wrote to.
    #[test]
    fn gnu_strerror_r_returns_the_message() {
        // SAFETY: the address is `gnu_strerror_r`, which has this signature.
        let f: Gnu = unsafe { std::mem::transmute(find("__gnu_strerror_r")) };
        let mut buf = [0x7f as c_char; 256];
        // SAFETY: `buf` is 256 bytes and the length passed says so.
        unsafe {
            let got = f(22, buf.as_mut_ptr(), buf.len());
            assert!(!got.is_null());
            assert_eq!(CStr::from_ptr(got).to_str().unwrap(), "Invalid argument");
        }
    }
}

#[cfg(test)]
mod netdb_tests {
    use super::*;

    /// `struct addrinfo` as Roblox sees it: `ai_canonname` before `ai_addr`,
    /// the reverse of glibc's.
    #[repr(C)]
    struct BionicAddrinfo {
        ai_flags: c_int,
        ai_family: c_int,
        ai_socktype: c_int,
        ai_protocol: c_int,
        ai_addrlen: u32,
        ai_canonname: *mut c_char,
        ai_addr: *mut u8,
        ai_next: *mut BionicAddrinfo,
    }

    type Getaddrinfo = unsafe extern "C" fn(
        *const c_char,
        *const c_char,
        *const BionicAddrinfo,
        *mut *mut BionicAddrinfo,
    ) -> c_int;
    type Freeaddrinfo = unsafe extern "C" fn(*mut BionicAddrinfo);

    fn find(name: &str) -> usize {
        function_overrides()
            .into_iter()
            .find(|(n, _)| *n == name)
            .unwrap_or_else(|| panic!("{name} is not overridden"))
            .1 as usize
    }

    fn gai() -> Getaddrinfo {
        // SAFETY: the address is `cordial_getaddrinfo`, which has this signature.
        unsafe { std::mem::transmute::<usize, Getaddrinfo>(find("getaddrinfo")) }
    }

    fn freeai() -> Freeaddrinfo {
        // SAFETY: the address is `cordial_freeaddrinfo`, which has this signature.
        unsafe { std::mem::transmute::<usize, Freeaddrinfo>(find("freeaddrinfo")) }
    }

    /// bionic's `AI_DEFAULT` (`AI_V4MAPPED_CFG | AI_ADDRCONFIG`), the value that
    /// made glibc answer `EAI_BADFLAGS` for every lookup before the shim.
    const BIONIC_AI_DEFAULT: c_int = 0x600;
    const BIONIC_AI_NUMERICHOST: c_int = 0x4;

    fn lookup_numeric(gai: Getaddrinfo, free: Freeaddrinfo) {
        let hints = BionicAddrinfo {
            ai_flags: BIONIC_AI_DEFAULT | BIONIC_AI_NUMERICHOST,
            ai_family: 0,
            ai_socktype: 1, // SOCK_STREAM
            ai_protocol: 0,
            ai_addrlen: 0,
            ai_canonname: std::ptr::null_mut(),
            ai_addr: std::ptr::null_mut(),
            ai_next: std::ptr::null_mut(),
        };
        let mut res: *mut BionicAddrinfo = std::ptr::null_mut();
        // SAFETY: both strings are NUL-terminated literals, `hints` is a live
        // bionic-layout struct, and `res` receives the list this frees below.
        let rc = unsafe { gai(c"127.0.0.1".as_ptr(), c"443".as_ptr(), &hints, &mut res) };
        assert_eq!(rc, 0);
        assert!(!res.is_null());
        // SAFETY: a successful call returns a list of bionic-layout nodes that
        // this shim allocated and `free` releases.
        unsafe {
            let a = &*res;
            assert_eq!(a.ai_family, 2, "AF_INET");
            assert_eq!(a.ai_addrlen, 16);
            assert!(a.ai_canonname.is_null(), "no AI_CANONNAME was asked for");
            assert!(!a.ai_addr.is_null(), "ai_addr must be the sockaddr, not the name");
            let sa = std::slice::from_raw_parts(a.ai_addr, 16);
            assert_eq!(u16::from_ne_bytes([sa[0], sa[1]]), 2, "sa_family");
            assert_eq!(u16::from_be_bytes([sa[2], sa[3]]), 443, "port");
            assert_eq!(&sa[4..8], &[127, 0, 0, 1]);
            free(res);
        }
    }

    #[test]
    fn a_numeric_lookup_returns_the_bionic_layout() {
        lookup_numeric(gai(), freeai());
    }

    #[test]
    fn a_bad_numeric_host_reports_the_bionic_eai_noname() {
        let gai = gai();
        let hints = BionicAddrinfo {
            ai_flags: BIONIC_AI_NUMERICHOST,
            ai_family: 0,
            ai_socktype: 1,
            ai_protocol: 0,
            ai_addrlen: 0,
            ai_canonname: std::ptr::null_mut(),
            ai_addr: std::ptr::null_mut(),
            ai_next: std::ptr::null_mut(),
        };
        let mut res: *mut BionicAddrinfo = std::ptr::dangling_mut();
        // SAFETY: as above.
        let rc = unsafe { gai(c"not-an-address".as_ptr(), std::ptr::null(), &hints, &mut res) };
        assert_eq!(rc, 8, "bionic EAI_NONAME, not glibc's -2");
        assert!(res.is_null(), "a failed lookup must not leave a stale list behind");
    }

    /// The engine resolves from several `HttpClient` threads at once. The shim
    /// keeps no shared state, so this is a check that it stays that way: every
    /// lookup on every thread must come back whole.
    #[test]
    fn concurrent_lookups_each_come_back_whole() {
        let (gai, free) = (gai(), freeai());
        let threads: Vec<_> = (0..8)
            .map(|_| std::thread::spawn(move || (0..300).for_each(|_| lookup_numeric(gai, free))))
            .collect();
        for t in threads {
            t.join().expect("a lookup thread panicked");
        }
    }
}
