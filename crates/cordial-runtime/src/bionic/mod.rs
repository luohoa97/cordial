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
        // FORTIFY wrappers.
        f!("__strlen_chk", strlen_chk),
        f!("__strchr_chk", strchr_chk),
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
    // OpenSL ES. Data symbols, so a missing one fails the DT_NEEDED walk rather
    // than the first audio call — see `opensles_overrides`.
    v.extend(opensles_overrides());
    // `pthread_create`, forwarded untouched unless `CORDIAL_TRACE_THREADS=1` —
    // see `thread_overrides` and `native/thread_trace.cpp`.
    v.extend(thread_overrides());
    if std::env::var_os("CORDIAL_TRACE_PATHS").is_some() {
        extern "C" {
            fn cordial_set_path_trace(on: c_int);
        }
        // SAFETY: sets one bool in system_paths.cpp.
        unsafe { cordial_set_path_trace(1) };
    }
    if std::env::var_os("CORDIAL_TRACE_THREADS").is_some() {
        extern "C" {
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

extern "C" {
    fn __errno_location() -> *mut c_int;
    fn strlen(s: *const c_char) -> usize;
    fn strchr(s: *const c_char, c: c_int) -> *mut c_char;
    fn strncpy(dst: *mut c_char, src: *const c_char, n: usize) -> *mut c_char;
    fn strerror_r(errnum: c_int, buf: *mut c_char, buflen: usize) -> c_int;
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
    let addr = (*FOUND.get_or_init(|| {
        // SAFETY: a plain symbol lookup in the already-loaded host libc.
        let p = unsafe {
            libc_dlsym(std::ptr::null_mut(), c"mallinfo2".as_ptr())
        };
        if p.is_null() { None } else { Some(p as usize) }
    }))?;
    // SAFETY: `mallinfo2` takes no arguments and returns the ten-`size_t`
    // struct `BionicMallinfo` mirrors exactly.
    Some(unsafe { std::mem::transmute::<usize, extern "C" fn() -> BionicMallinfo>(addr) })
}

/// **Reports the host allocator's arena, which may not be the engine's.**
///
/// `mimalloc_lib` exists, so whether these numbers describe the heap Roblox is
/// actually allocating from depends on what ends up overriding `malloc` in a
/// given run. They are the truth about glibc's arena either way, and that is
/// what the name promises; nothing here invents a number. `INFERRED` that the
/// engine only uses this for telemetry -- no behaviour change was observed when
/// it went from a stub to this.
extern "C" fn bionic_mallinfo() -> BionicMallinfo {
    match host_mallinfo2() {
        Some(f) => f(),
        None => BionicMallinfo {
            arena: 0, ordblks: 0, smblks: 0, hblks: 0, hblkhd: 0,
            usmblks: 0, fsmblks: 0, uordblks: 0, fordblks: 0, keepcost: 0,
        },
    }
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
extern "C" fn bionic_cxa_thread_atexit_impl(
    func: *mut c_void,
    arg: *mut c_void,
    dso_handle: *mut c_void,
) -> c_int {
    // SAFETY: the three pointers are passed straight through to the host
    // implementation of the same function, with the same signature.
    unsafe { host_cxa_thread_atexit_impl(func, arg, dso_handle) }
}

extern "C" fn bionic_errno() -> *mut c_int {
    // SAFETY: glibc's per-thread errno slot; the same contract as bionic's.
    unsafe { __errno_location() }
}

unsafe fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        "(null)".into()
    } else {
        CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}

extern "C" fn bionic_assert(file: *const c_char, line: c_int, msg: *const c_char) -> ! {
    // SAFETY: bionic passes NUL-terminated strings or null.
    unsafe {
        eprintln!("\n*** Roblox assertion failed ***");
        eprintln!("    {}:{}: {}", cstr(file), line, cstr(msg));
    }
    crate::stubs::report();
    std::process::abort();
}

extern "C" fn bionic_assert2(
    file: *const c_char,
    line: c_int,
    function: *const c_char,
    failed: *const c_char,
) -> ! {
    // SAFETY: bionic passes NUL-terminated strings or null.
    unsafe {
        eprintln!("\n*** Roblox assertion failed ***");
        eprintln!("    {}:{} in {}", cstr(file), line, cstr(function));
        eprintln!("    assertion: {}", cstr(failed));
    }
    crate::stubs::report();
    std::process::abort();
}

/// bionic's GNU-flavoured `strerror_r`, which returns `char*` rather than int.
extern "C" fn gnu_strerror_r(errnum: c_int, buf: *mut c_char, buflen: usize) -> *mut c_char {
    // SAFETY: caller-supplied buffer of at least `buflen` bytes.
    unsafe {
        strerror_r(errnum, buf, buflen);
    }
    buf
}

extern "C" fn strlen_chk(s: *const c_char, _bound: usize) -> usize {
    unsafe { strlen(s) }
}

extern "C" fn strchr_chk(s: *const c_char, c: c_int, _bound: usize) -> *mut c_char {
    unsafe { strchr(s, c) }
}

extern "C" fn strncpy_chk2(
    dst: *mut c_char,
    src: *const c_char,
    n: usize,
    _dst_len: usize,
    _src_len: usize,
) -> *mut c_char {
    unsafe { strncpy(dst, src, n) }
}

extern "C" fn write_chk(fd: c_int, buf: *const c_void, count: usize, _bound: usize) -> isize {
    unsafe { write(fd, buf, count) }
}

extern "C" fn umask_chk(mode: u32) -> u32 {
    unsafe { umask(mode) }
}

extern "C" fn poll_chk(fds: *mut c_void, nfds: u64, timeout: c_int, _bound: usize) -> c_int {
    unsafe { poll(fds, nfds, timeout) }
}

extern "C" fn readlink_chk(
    path: *const c_char,
    buf: *mut c_char,
    size: usize,
    _bound: usize,
) -> isize {
    unsafe { readlink(path, buf, size) }
}

extern "C" fn fread_chk(
    ptr: *mut c_void,
    size: usize,
    n: usize,
    stream: *mut c_void,
    _bound: usize,
) -> usize {
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
extern "C" fn android_get_device_api_level() -> c_int {
    33
}

/// bionic's `__system_property_get(name, value)`. `value` is a caller buffer of
/// `PROP_VALUE_MAX` (92) bytes; the return is the length written.
extern "C" fn system_property_get(name: *const c_char, value: *mut c_char) -> c_int {
    const PROP_VALUE_MAX: usize = 92;
    if value.is_null() {
        return 0;
    }
    // SAFETY: bionic's contract is a NUL-terminated name and a 92-byte buffer.
    let key = unsafe { cstr(name) };
    let found = PROPERTIES
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| *v)
        .unwrap_or("");

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

    let bytes = found.as_bytes();
    let n = bytes.len().min(PROP_VALUE_MAX - 1);
    // SAFETY: `value` has at least PROP_VALUE_MAX bytes per the ABI, and n < that.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), value as *mut u8, n);
        *value.add(n) = 0;
    }
    n as c_int
}

// ---------------------------------------------------------------------- sysconf

pub mod sysconf_table;

extern "C" {
    fn sysconf(name: c_int) -> i64;
}

/// bionic's `sysconf`, translated.
///
/// The selectors are not shared: bionic's `_SC_PAGESIZE` is 39, glibc's is 30,
/// and glibc reads 39 as `_SC_BC_STRING_MAX`. Roblox asks for the page size
/// during static initialisation and, untranslated, is told 1000 — which is not a
/// power of two, so the allocator that asked aborts. Nothing about that failure
/// points back at `sysconf`, which is why the translation is worth its table.
extern "C" fn bionic_sysconf(name: c_int) -> i64 {
    if let Some(&(_, glibc, _)) = sysconf_table::SYSCONF_MAP
        .iter()
        .find(|(bionic, _, _)| *bionic == name)
    {
        // SAFETY: `glibc` came from the generated table and is a valid selector.
        return unsafe { sysconf(glibc) };
    }

    if let Some((_, sym)) = sysconf_table::UNSUPPORTED
        .iter()
        .find(|(bionic, _)| *bionic == name)
    {
        eprintln!("[bionic] sysconf({sym}) has no glibc equivalent; returning -1");
        return -1;
    }

    eprintln!("[bionic] sysconf({name}) is not a selector bionic defines; returning -1");
    -1
}

// ----------------------------------------------------------------------- liblog

/// Android's `liblog`, implemented in `native/liblog.cpp`.
///
/// Roblox narrates its own startup through these, so wiring them up is what turns
/// every later failure from silent into explained. Implemented in C++ because
/// three of the six are variadic.
pub fn liblog_overrides() -> Vec<(&'static str, *mut c_void)> {
    #[repr(C)]
    struct Symbol {
        name: *const c_char,
        addr: *mut c_void,
    }
    extern "C" {
        fn cordial_liblog_symbols(count: *mut usize) -> *const Symbol;
    }

    let mut count = 0usize;
    // SAFETY: the table is a static in liblog.cpp and outlives the process.
    let table = unsafe { cordial_liblog_symbols(&mut count) };
    if table.is_null() {
        return Vec::new();
    }
    // SAFETY: `table` points at `count` initialised entries with static names.
    let entries = unsafe { std::slice::from_raw_parts(table, count) };
    entries
        .iter()
        .map(|e| {
            // SAFETY: each `name` is a string literal in liblog.cpp.
            let name = unsafe { CStr::from_ptr(e.name) }.to_str().unwrap_or("");
            (name, e.addr)
        })
        .collect()
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
    #[repr(C)]
    struct Symbol {
        name: *const c_char,
        addr: *mut c_void,
    }
    extern "C" {
        fn cordial_opensles_symbols(count: *mut usize) -> *const Symbol;
    }

    let mut count = 0usize;
    // SAFETY: the table is a static in opensles.cpp and outlives the process.
    let table = unsafe { cordial_opensles_symbols(&mut count) };
    if table.is_null() {
        return Vec::new();
    }
    // SAFETY: `table` points at `count` initialised entries with static names.
    let entries = unsafe { std::slice::from_raw_parts(table, count) };
    entries
        .iter()
        .map(|e| {
            // SAFETY: each `name` is a string literal in opensles.cpp.
            let name = unsafe { CStr::from_ptr(e.name) }.to_str().unwrap_or("");
            (name, e.addr)
        })
        .collect()
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
    extern "C" {
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
    extern "C" {
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
    #[repr(C)]
    struct Symbol {
        name: *const c_char,
        addr: *mut c_void,
    }
    extern "C" {
        fn cordial_aaudio_symbols(count: *mut usize) -> *const Symbol;
    }

    let mut count = 0usize;
    // SAFETY: the table is a static in aaudio.cpp and outlives the process.
    let table = unsafe { cordial_aaudio_symbols(&mut count) };
    if table.is_null() {
        return Vec::new();
    }
    // SAFETY: `table` points at `count` initialised entries with static names.
    let entries = unsafe { std::slice::from_raw_parts(table, count) };
    entries
        .iter()
        .map(|e| {
            // SAFETY: each `name` is a string literal in aaudio.cpp.
            let name = unsafe { CStr::from_ptr(e.name) }.to_str().unwrap_or("");
            (name, e.addr)
        })
        .collect()
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
    #[repr(C)]
    struct Symbol {
        name: *const c_char,
        addr: *mut c_void,
    }
    extern "C" {
        fn cordial_system_symbols(count: *mut usize) -> *const Symbol;
    }

    let mut count = 0usize;
    // SAFETY: the table is a static in system_paths.cpp and outlives the process.
    let table = unsafe { cordial_system_symbols(&mut count) };
    if table.is_null() {
        return Vec::new();
    }
    // SAFETY: `table` points at `count` initialised entries with static names.
    let entries = unsafe { std::slice::from_raw_parts(table, count) };
    entries
        .iter()
        .map(|e| {
            // SAFETY: each `name` is a string literal in system_paths.cpp.
            let name = unsafe { CStr::from_ptr(e.name) }.to_str().unwrap_or("");
            (name, e.addr)
        })
        .collect()
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
    #[repr(C)]
    struct Symbol {
        name: *const c_char,
        addr: *mut c_void,
    }
    extern "C" {
        fn cordial_thread_symbols(count: *mut usize) -> *const Symbol;
    }

    let mut count = 0usize;
    // SAFETY: the table is a static in thread_trace.cpp and outlives the process.
    let table = unsafe { cordial_thread_symbols(&mut count) };
    if table.is_null() {
        return Vec::new();
    }
    // SAFETY: `table` points at `count` initialised entries with static names.
    let entries = unsafe { std::slice::from_raw_parts(table, count) };
    entries
        .iter()
        .map(|e| {
            // SAFETY: each `name` is a string literal in thread_trace.cpp.
            let name = unsafe { CStr::from_ptr(e.name) }.to_str().unwrap_or("");
            (name, e.addr)
        })
        .collect()
}

// ------------------------------------------------------------------- netdb

/// `getaddrinfo`/`freeaddrinfo` in bionic's layout — `native/netdb_compat.cpp`.
///
/// Without these the engine cannot resolve a single hostname: bionic's
/// `AI_DEFAULT` sets a bit glibc rejects with `EAI_BADFLAGS`, and a result that
/// did come back would have its `ai_addr` and `ai_canonname` transposed.
pub fn netdb_overrides() -> Vec<(&'static str, *mut c_void)> {
    #[repr(C)]
    struct Symbol {
        name: *const c_char,
        addr: *mut c_void,
    }
    extern "C" {
        fn cordial_netdb_symbols(count: *mut usize) -> *const Symbol;
    }

    let mut count = 0usize;
    // SAFETY: the table is a static in netdb_compat.cpp and outlives the process.
    let table = unsafe { cordial_netdb_symbols(&mut count) };
    if table.is_null() {
        return Vec::new();
    }
    // SAFETY: `table` points at `count` initialised entries with static names.
    let entries = unsafe { std::slice::from_raw_parts(table, count) };
    entries
        .iter()
        .map(|e| {
            // SAFETY: each `name` is a string literal in netdb_compat.cpp.
            let name = unsafe { CStr::from_ptr(e.name) }.to_str().unwrap_or("");
            (name, e.addr)
        })
        .collect()
}
