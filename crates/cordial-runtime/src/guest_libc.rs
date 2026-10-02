//! Hand-written answers to the arm64 guest's libc imports, where the generic
//! call builder cannot be trusted with them (docs/vr/dynarmic-design.md
//! §3.1): the function takes or runs a guest callback, describes the CPU,
//! walks a `va_list`, or passes a structure bionic lays out differently on
//! arm64 than glibc does on x86-64.
//!
//! Each one either does what bionic would have done for an arm64 process, or
//! returns the failure the function documents. None of them returns a
//! success it did not have: that is the rule `guest_link` states for stops,
//! applied to the thunks that replace them.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{c_char, c_int, c_void, CStr};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use cordial_guest::{guest_call, Call, Fault, Handler, Ret, Runtime, Ty};

/// How many times each hand-written thunk ran, for the end-of-run report.
static COUNTS: Mutex<BTreeMap<&'static str, u64>> = Mutex::new(BTreeMap::new());

pub(crate) fn count(name: &'static str) {
    *COUNTS.lock().unwrap().entry(name).or_default() += 1;
}

/// Prints how often each thunk ran.
pub fn report() {
    let c = COUNTS.lock().unwrap();
    if c.is_empty() {
        return;
    }
    let line: Vec<String> = c.iter().map(|(n, k)| format!("{n}={k}")).collect();
    println!("guest thunk calls: {}", line.join(" "));
}

pub(crate) fn errno_set(e: c_int) {
    extern "C" {
        fn __errno_location() -> *mut c_int;
    }
    // SAFETY: the calling thread's own errno; the guest reads it through the
    // same location (`__errno` dispatches to Cordial's, which returns it).
    unsafe { *__errno_location() = e };
}

const ENOENT: c_int = 2;

// <asm/hwcap.h> for arm64.
const HWCAP_FP: u64 = 1 << 0;
const HWCAP_ASIMD: u64 = 1 << 1;
const HWCAP_AES: u64 = 1 << 3;
const HWCAP_PMULL: u64 = 1 << 4;
const HWCAP_SHA1: u64 = 1 << 5;
const HWCAP_SHA2: u64 = 1 << 6;
const HWCAP_CRC32: u64 = 1 << 7;
const HWCAP_ATOMICS: u64 = 1 << 8;
const HWCAP_FCMA: u64 = 1 << 14;
const HWCAP_ASIMDDP: u64 = 1 << 20;

/// What the emulated CPU implements, as `AT_HWCAP` reports it (design §1.3).
///
/// Each bit is an instruction family dynarmic's A64 decoder has live
/// entries for (`frontend/A64/decoder/a64.inc`): AES, PMULL, SHA1/SHA256,
/// CRC32, SDOT/UDOT and FCMLA/FCADD. LSE atomics, FP16 arithmetic, RDM,
/// JSCVT, LRCPC, SVE, PAC and RNG are commented out of the decoder, so they
/// are absent here: the engine asks and takes its fallback paths itself.
/// `CPUID` is absent too, because nothing emulates the kernel's trapping of
/// ID-register reads. This is not a lie about the host; it is the CPU the
/// guest runs on.
///
/// `CORDIAL_GUEST_HWCAP_ATOMICS=1` adds `ATOMICS`, which *is* false: it is
/// the control design §8 names for M3, and should stop the guest in an
/// outline-atomics helper.
pub fn guest_hwcap() -> u64 {
    let honest = HWCAP_FP | HWCAP_ASIMD | HWCAP_AES | HWCAP_PMULL | HWCAP_SHA1 | HWCAP_SHA2
        | HWCAP_CRC32 | HWCAP_FCMA | HWCAP_ASIMDDP;
    if std::env::var_os("CORDIAL_GUEST_HWCAP_ATOMICS").is_some() {
        honest | HWCAP_ATOMICS
    } else {
        honest
    }
}

const AT_PAGESZ: u64 = 6;
const AT_CLKTCK: u64 = 17;
const AT_UID: u64 = 11;
const AT_EUID: u64 = 12;
const AT_GID: u64 = 13;
const AT_EGID: u64 = 14;
const AT_PLATFORM: u64 = 15;
const AT_HWCAP: u64 = 16;
const AT_SECURE: u64 = 23;
const AT_RANDOM: u64 = 25;
const AT_HWCAP2: u64 = 26;

fn getauxval() -> Handler {
    extern "C" {
        #[link_name = "getauxval"]
        fn host_getauxval(t: u64) -> u64;
    }
    Box::new(|c| {
        count("getauxval");
        let t = c.x(0);
        let v = match t {
            AT_HWCAP => Some(guest_hwcap()),
            // No HWCAP2 feature is implemented by the translator that the
            // engine could use; FRINT, I8MM, BF16 and the rest are all
            // commented out of the decoder.
            AT_HWCAP2 => Some(0),
            AT_PAGESZ => Some(4096),
            AT_CLKTCK => Some(100),
            AT_PLATFORM => Some(c"aarch64".as_ptr() as u64),
            // The process's own, and they mean the same thing to either
            // instruction set: ids, the secure-mode bit, and the address of
            // sixteen random bytes.
            // SAFETY: plain libc call.
            AT_UID | AT_EUID | AT_GID | AT_EGID | AT_SECURE | AT_RANDOM => Some(unsafe { host_getauxval(t) }),
            _ => None,
        };
        match v {
            Some(v) => c.set_x(0, v),
            // bionic: 0 and ENOENT for a type the kernel did not supply.
            None => {
                errno_set(ENOENT);
                c.set_x(0, 0);
            }
        }
        Ok(())
    })
}

/// `uname`: the host's, with `machine` saying what the guest runs on.
/// `struct utsname` is six 65-byte fields in bionic and in glibc alike.
fn uname() -> Handler {
    extern "C" {
        #[link_name = "uname"]
        fn host_uname(u: *mut u8) -> c_int;
    }
    Box::new(|c| {
        count("uname");
        let u = c.x(0) as *mut u8;
        // SAFETY: the guest's struct utsname, 390 bytes on both sides.
        let r = unsafe { host_uname(u) };
        if r == 0 {
            // SAFETY: field 4 (`machine`) of the same struct.
            unsafe {
                let m = u.add(4 * 65);
                std::ptr::write_bytes(m, 0, 65);
                std::ptr::copy_nonoverlapping(b"aarch64".as_ptr(), m, 7);
            }
        }
        c.set_x(0, r as u32 as u64);
        Ok(())
    })
}

/// One `__cxa_atexit` registration: `f(arg)`, for the object `dso`.
#[derive(Clone, Copy)]
struct AtExit {
    f: u64,
    arg: u64,
    dso: u64,
}

/// Guest `__cxa_atexit` registrations, in order. bionic runs them from
/// `exit()` and `__cxa_finalize`; on Android an app process is killed far
/// more often than it exits, so for most of them nothing ever does.
static ATEXIT: Mutex<Vec<AtExit>> = Mutex::new(Vec::new());

fn cxa_atexit() -> Handler {
    Box::new(|c| {
        count("__cxa_atexit");
        ATEXIT.lock().unwrap().push(AtExit { f: c.x(0), arg: c.x(1), dso: c.x(2) });
        c.set_x(0, 0);
        Ok(())
    })
}

/// Runs, in reverse order of registration, every handler registered for
/// `dso` (or all of them for a null `dso`), each once, as bionic's
/// `__cxa_finalize` does.
fn run_atexit(rt: &Arc<Runtime>, dso: u64) -> Result<(), Fault> {
    loop {
        let next = {
            let mut l = ATEXIT.lock().unwrap();
            let i = l.iter().rposition(|e| dso == 0 || e.dso == dso);
            i.map(|i| l.remove(i))
        };
        let Some(e) = next else { return Ok(()) };
        guest_call(rt, e.f, &[e.arg], &[])?;
    }
}

fn cxa_finalize() -> Handler {
    Box::new(|c| {
        count("__cxa_finalize");
        run_atexit(c.runtime(), c.x(0))
    })
}

thread_local! {
    /// Registered `thread_local` destructors per host thread, for M4's
    /// guest thread exit. Nothing runs them on the main thread, which does
    /// not exit while the engine lives.
    static THREAD_ATEXIT: RefCell<Vec<AtExit>> = const { RefCell::new(Vec::new()) };
}

/// What bionic does as a thread leaves: its `thread_local` destructors in
/// reverse order of registration (`__cxa_thread_finalize`), then each key's
/// destructor for a non-null value, repeated while any ran, up to
/// `PTHREAD_DESTRUCTOR_ITERATIONS` (4) times (`pthread_key_clean_all`). Run
/// by a guest thread's start routine after the guest's own returns or calls
/// `pthread_exit`, on the thread itself, so each value read is its own.
pub(crate) fn thread_exit(rt: &Arc<Runtime>) -> Result<(), Fault> {
    extern "C" {
        fn pthread_getspecific(k: u32) -> *mut c_void;
        fn pthread_setspecific(k: u32, v: *const c_void) -> c_int;
    }
    while let Some(e) = THREAD_ATEXIT.with(|t| t.borrow_mut().pop()) {
        guest_call(rt, e.f, &[e.arg], &[])?;
    }
    if guest_keys() {
        return cordial_guest::key_clean_all(rt);
    }
    for _ in 0..4 {
        let keys: Vec<(u32, u64)> = KEY_DTORS.lock().unwrap().iter().map(|(k, d)| (*k, *d)).collect();
        let mut ran = false;
        for (k, dtor) in keys {
            // SAFETY: a key this process created; the value is this thread's.
            let v = unsafe { pthread_getspecific(k) };
            if !v.is_null() {
                // SAFETY: as above.
                unsafe { pthread_setspecific(k, std::ptr::null()) };
                guest_call(rt, dtor, &[v as u64], &[])?;
                ran = true;
            }
        }
        if !ran {
            break;
        }
    }
    Ok(())
}

fn cxa_thread_atexit_impl() -> Handler {
    Box::new(|c| {
        count("__cxa_thread_atexit_impl");
        THREAD_ATEXIT.with(|t| t.borrow_mut().push(AtExit { f: c.x(0), arg: c.x(1), dso: c.x(2) }));
        c.set_x(0, 0);
        Ok(())
    })
}

/// `pthread_once`. bionic's `pthread_once_t` is an int holding 0 (not
/// started), 1 (underway) or 2 (complete); the guest's initialiser runs on
/// the next Jit, and a thread that finds another one underway waits for it
/// to complete, as bionic's futex wait does.
fn pthread_once() -> Handler {
    Box::new(|c| {
        count("pthread_once");
        // SAFETY: the guest's pthread_once_t, an aligned int.
        let once = unsafe { &*(c.x(0) as *const AtomicI32) };
        let init = c.x(1);
        loop {
            match once.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => {
                    guest_call(c.runtime(), init, &[], &[])?;
                    once.store(2, Ordering::Release);
                    break;
                }
                Err(2) => break,
                Err(_) => std::thread::yield_now(),
            }
        }
        c.set_x(0, 0);
        Ok(())
    })
}

/// Whether the guest's pthread keys are bionic's, kept in guest-visible
/// memory with `pthread_getspecific`/`setspecific` run as guest code
/// (`cordial_guest::keys`), or host glibc keys behind stubs, as before M7.
/// `CORDIAL_GUEST_TLS_KEYS=host` is the control. Decided once, at link
/// time: the four functions must agree on what a key is.
pub fn guest_keys() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("CORDIAL_GUEST_TLS_KEYS").as_deref() != Ok("host"))
}

/// Whether `memcpy`, `memmove`, `memset` and `memcmp` run as guest code up
/// to 128 bytes (`cordial_guest::string`), or always through their stubs.
/// `CORDIAL_GUEST_STRING=host` is the control.
pub fn guest_string() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("CORDIAL_GUEST_STRING").as_deref() != Ok("host"))
}

/// Whether `clock_gettime(CLOCK_MONOTONIC)` runs as guest code over
/// CNTPCT_EL0 (`cordial_guest::clock`). `CORDIAL_GUEST_CLOCK=host` is the
/// control.
pub fn guest_clock() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("CORDIAL_GUEST_CLOCK").as_deref() != Ok("host"))
}

fn guest_key_create() -> Handler {
    Box::new(|c| {
        count("pthread_key_create");
        let r = match c.runtime().key_create(c.x(1)) {
            Ok(k) => {
                // SAFETY: bionic's pthread_key_t is an int; the guest passed
                // its address.
                unsafe { (c.x(0) as *mut u32).write(k) };
                0
            }
            Err(e) => e,
        };
        c.set_x(0, r as u32 as u64);
        Ok(())
    })
}

fn guest_key_delete() -> Handler {
    Box::new(|c| {
        count("pthread_key_delete");
        c.set_x(0, c.runtime().key_delete(c.x(0) as u32) as u32 as u64);
        Ok(())
    })
}

/// Guest destructors for keys made by `pthread_key_create`, by key. The
/// host key is created without one: glibc would call it as x86 code. M4's
/// guest thread exit runs these instead.
static KEY_DTORS: Mutex<BTreeMap<u32, u64>> = Mutex::new(BTreeMap::new());

fn pthread_key_create() -> Handler {
    extern "C" {
        #[link_name = "pthread_key_create"]
        fn host_key_create(k: *mut u32, d: Option<extern "C" fn(*mut c_void)>) -> c_int;
    }
    Box::new(|c| {
        count("pthread_key_create");
        let out = c.x(0) as *mut u32;
        let dtor = c.x(1);
        let mut k = 0u32;
        // SAFETY: plain libc call into a local.
        let r = unsafe { host_key_create(&mut k, None) };
        if r == 0 {
            if dtor != 0 {
                KEY_DTORS.lock().unwrap().insert(k, dtor);
            }
            // SAFETY: bionic's pthread_key_t is an int; the guest passed its
            // address.
            unsafe { out.write(k) };
        }
        c.set_x(0, r as u32 as u64);
        Ok(())
    })
}

/// `bsearch`, run here rather than by glibc because the comparator is guest
/// code and glibc's has no context argument to reach it through. The same
/// algorithm POSIX describes: any matching element may be returned.
fn bsearch() -> Handler {
    Box::new(|c| {
        count("bsearch");
        let (key, base, n, size, cmp) = (c.x(0), c.x(1), c.x(2), c.x(3), c.x(4));
        let (mut lo, mut hi) = (0u64, n);
        let mut found = 0u64;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let p = base + mid * size;
            let r = guest_call(c.runtime(), cmp, &[key, p], &[])?.x0 as i32;
            if r == 0 {
                found = p;
                break;
            } else if r < 0 {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        c.set_x(0, found);
        Ok(())
    })
}

// The bionic linker's own entry points, which the native path's libdl.so
// wraps. The caller address decides the namespace and RTLD_NEXT.
extern "C" {
    fn __loader_dlopen(filename: *const c_char, flags: c_int, caller: *const c_void) -> *mut c_void;
    fn __loader_dlsym(handle: *mut c_void, symbol: *const c_char, caller: *const c_void) -> *mut c_void;
    fn __loader_dladdr(addr: *const c_void, info: *mut [u64; 4]) -> c_int;
    fn __loader_dlclose(handle: *mut c_void) -> c_int;
    fn __loader_dlerror() -> *mut c_char;
    fn __loader_dl_iterate_phdr(
        cb: extern "C" fn(info: *mut DlPhdrInfo, size: usize, data: *mut c_void) -> c_int,
        data: *mut c_void,
    ) -> c_int;
}

/// `struct dl_phdr_info`: the same 64 bytes on LP64 bionic and glibc.
#[repr(C)]
#[derive(Clone, Copy)]
struct DlPhdrInfo {
    addr: u64,
    name: *const c_char,
    phdr: *const u8,
    phnum: u16,
    adds: u64,
    subs: u64,
    tls_modid: u64,
    tls_data: *mut c_void,
}

const EM_AARCH64: u16 = 183;

/// Whether the object loaded at `base` is an arm64 ELF, i.e. guest code.
fn is_guest_object(base: u64) -> bool {
    if base == 0 {
        return false;
    }
    // SAFETY: `base` is where the linker mapped an object's first segment,
    // which starts with its ELF header.
    unsafe {
        let h = base as *const u8;
        std::slice::from_raw_parts(h, 4) == b"\x7fELF" && (h.add(18) as *const u16).read_unaligned() == EM_AARCH64
    }
}

/// Addresses the guest's imports resolve to as data, which a guest `dlsym`
/// may also hand back. Filled by `guest_link::build`.
pub static GUEST_DATA: OnceLock<BTreeSet<u64>> = OnceLock::new();

thread_local! {
    /// A `dlerror` message of this layer's own, taking precedence over the
    /// linker's until read.
    static DLERROR: RefCell<Option<std::ffi::CString>> = const { RefCell::new(None) };
}

fn set_dlerror(msg: String) {
    DLERROR.with(|d| *d.borrow_mut() = std::ffi::CString::new(msg).ok());
}

/// `CORDIAL_TRACE_DLSYM=1`, the same switch and the same line as the native
/// path's `patches/0002`. That patch lives in the linker's `libdl.so`
/// wrapper, which the guest never passes through -- these thunks call the
/// `__loader_*` entries directly -- so without this the switch was silently
/// inert under the translator, and "which audio library does the Quest's
/// FMOD look for" had no way to be answered by observation.
fn trace_dlsym() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("CORDIAL_TRACE_DLSYM").is_some())
}

fn guest_cstr(p: u64) -> String {
    if p == 0 {
        return "(null)".into();
    }
    // SAFETY: a C string the guest passed.
    unsafe { CStr::from_ptr(p as *const c_char) }.to_string_lossy().into_owned()
}

fn dlopen() -> Handler {
    Box::new(|c| {
        count("dlopen");
        // bionic's RTLD_* values on arm64 are the generic ones glibc also
        // uses (RTLD_NOW 2, RTLD_GLOBAL 0x100, RTLD_NOLOAD 4), and it is
        // bionic's linker that reads them.
        // SAFETY: the guest's filename, or null for the main program.
        let h = unsafe { __loader_dlopen(c.x(0) as *const c_char, c.x(1) as c_int, c.x(30) as *const c_void) };
        if trace_dlsym() {
            eprintln!("[cordial-dlsym-trace] guest dlopen({}, {}) -> {h:p}", guest_cstr(c.x(0)), c.x(1) as c_int);
        }
        c.set_x(0, h as u64);
        Ok(())
    })
}

/// A guest `dlsym` may only ever return guest code or guest-visible data:
/// the linker's `RTLD_DEFAULT` search also sees host objects (its own
/// `libdl.so`), and a host function address handed to arm64 code would be
/// run as arm64 instructions.
fn dlsym() -> Handler {
    Box::new(|c| {
        count("dlsym");
        let (handle, sym) = (c.x(0) as *mut c_void, c.x(1) as *const c_char);
        // SAFETY: the guest's handle and symbol name, for the linker to check.
        let p = unsafe { __loader_dlsym(handle, sym, c.x(30) as *const c_void) } as u64;
        let ok = p == 0
            || c.runtime().stub_name(p).is_some()
            || GUEST_DATA.get().is_some_and(|d| d.contains(&p))
            || {
                let mut info = [0u64; 4];
                // SAFETY: a Dl_info-sized buffer of ours.
                (unsafe { __loader_dladdr(p as *const c_void, &mut info) }) != 0 && is_guest_object(info[1])
            };
        if trace_dlsym() {
            let shown = if ok { p } else { 0 };
            eprintln!("[cordial-dlsym-trace] guest dlsym({handle:p}, {}) -> {shown:#x}{}", guest_cstr(sym as u64),
                      if ok { "" } else { " (refused: host code)" });
        }
        if ok {
            c.set_x(0, p);
        } else {
            let name = guest_cstr(sym as u64);
            set_dlerror(format!("dlsym: {name} resolved only to host code, which the guest cannot call"));
            c.set_x(0, 0);
        }
        Ok(())
    })
}

fn dlerror() -> Handler {
    Box::new(|c| {
        count("dlerror");
        // Leaked per message, as the linker's own buffer is per thread and
        // reused: the guest may hold the pointer until its next dlerror.
        let own = DLERROR.with(|d| d.borrow_mut().take());
        let p = match own {
            Some(s) => s.into_raw() as u64,
            // SAFETY: the linker's thread-local message, or null.
            None => (unsafe { __loader_dlerror() }) as u64,
        };
        c.set_x(0, p);
        Ok(())
    })
}

fn dladdr() -> Handler {
    Box::new(|c| {
        count("dladdr");
        // Dl_info is four pointers on both sides.
        // SAFETY: the guest's address and its Dl_info.
        let r = unsafe { __loader_dladdr(c.x(0) as *const c_void, c.x(1) as *mut [u64; 4]) };
        c.set_x(0, r as u32 as u64);
        Ok(())
    })
}

fn dlclose() -> Handler {
    Box::new(|c| {
        count("dlclose");
        // SAFETY: the guest's handle, which the linker validates.
        let r = unsafe { __loader_dlclose(c.x(0) as *mut c_void) };
        c.set_x(0, r as u32 as u64);
        Ok(())
    })
}

/// `dl_iterate_phdr`, listing only guest objects. This is what the guest's
/// own libunwind uses to find `.eh_frame_hdr` for a PC when a C++ exception
/// is thrown (design §4), so the phdrs must be the guest's own. The virtual
/// libraries have none and are skipped; host objects are not the guest's
/// business. The list is taken first and the callbacks run after, each on
/// the next Jit.
fn dl_iterate_phdr() -> Handler {
    extern "C" fn collect(info: *mut DlPhdrInfo, _size: usize, data: *mut c_void) -> c_int {
        // SAFETY: the linker's info for one object, and our Vec.
        let (info, out) = unsafe { (&*info, &mut *(data as *mut Vec<DlPhdrInfo>)) };
        if info.phnum == 0 || info.phdr.is_null() {
            return 0;
        }
        // The ELF header sits at the load bias plus the first PT_LOAD's
        // vaddr, which for a shared object is the bias itself.
        let mut first_load = None;
        for i in 0..info.phnum as usize {
            // SAFETY: phnum entries of 56 bytes each.
            let ph = unsafe { info.phdr.add(i * 56) };
            // SAFETY: p_type at 0, p_offset at 8, p_vaddr at 16.
            let (ty, off, vaddr) = unsafe {
                ((ph as *const u32).read_unaligned(), (ph.add(8) as *const u64).read_unaligned(),
                 (ph.add(16) as *const u64).read_unaligned())
            };
            if ty == 1 && off == 0 {
                first_load = Some(vaddr);
                break;
            }
        }
        if first_load.is_some_and(|v| is_guest_object(info.addr + v)) {
            out.push(*info);
        }
        0
    }
    Box::new(|c| {
        count("dl_iterate_phdr");
        let (cb, data) = (c.x(0), c.x(1));
        let mut list: Vec<DlPhdrInfo> = Vec::new();
        // SAFETY: our callback and our Vec.
        unsafe { __loader_dl_iterate_phdr(collect, &mut list as *mut Vec<DlPhdrInfo> as *mut c_void) };
        let mut r = 0u64;
        for info in &list {
            let copy = Box::new(*info);
            let p = &*copy as *const DlPhdrInfo as u64;
            r = guest_call(c.runtime(), cb, &[p, std::mem::size_of::<DlPhdrInfo>() as u64, data], &[])?.x0
                as u32 as u64;
            if r != 0 {
                break;
            }
        }
        c.set_x(0, r);
        Ok(())
    })
}

// ------------------------------------------------------------------ locales
//
// bionic's locale model, which is not glibc's: a `locale_t` is a pointer to
// `{ size_t mb_cur_max }`, only "", "C", "POSIX", "C.UTF-8" and "en_US.UTF-8"
// exist, the process default is UTF-8, and every `_l` function ignores its
// locale argument (`bionic/locale.cpp`, `bionic/wctype.cpp` in the vendored
// tree). Multibyte conversion is always UTF-8. The LC_* numbers and masks,
// checked against both headers, are the same, so it is the objects and the
// behaviour that need answering here, not the constants.
//
// The host functions that do the work run under a glibc C.UTF-8 locale set
// on the calling thread for the duration of the call, because the host
// process's own locale is "C", in which glibc classifies and converts only
// ASCII and bionic would not.

const LC_ALL_MASK: u64 = 0x1fbf;
const LC_GLOBAL_LOCALE: u64 = u64::MAX;
const EINVAL: c_int = 22;

extern "C" {
    #[link_name = "newlocale"]
    fn host_newlocale(mask: c_int, name: *const c_char, base: *mut c_void) -> *mut c_void;
    #[link_name = "uselocale"]
    fn host_uselocale(l: *mut c_void) -> *mut c_void;
    #[link_name = "dlsym"]
    fn host_dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
}

struct HostLocale(*mut c_void);
// SAFETY: a glibc locale_t is immutable once made and may be used by any
// thread.
unsafe impl Send for HostLocale {}
// SAFETY: as above.
unsafe impl Sync for HostLocale {}

fn host_utf8() -> Option<*mut c_void> {
    static L: OnceLock<HostLocale> = OnceLock::new();
    let l = L.get_or_init(|| {
        // SAFETY: plain libc calls; glibc spells it both ways.
        let mut p = unsafe { host_newlocale(LC_ALL_MASK as c_int, c"C.UTF-8".as_ptr(), std::ptr::null_mut()) };
        if p.is_null() {
            // SAFETY: as above.
            p = unsafe { host_newlocale(LC_ALL_MASK as c_int, c"C.utf8".as_ptr(), std::ptr::null_mut()) };
        }
        HostLocale(p)
    });
    (!l.0.is_null()).then_some(l.0)
}

/// Runs `f` with the calling thread's host locale set to C.UTF-8.
fn under_utf8(name: &str, f: impl FnOnce() -> Result<(), Fault>) -> Result<(), Fault> {
    let Some(l) = host_utf8() else {
        return Err(Fault::Unsupported {
            thunk: name.into(),
            why: "the host has no C.UTF-8 locale to answer bionic's always-UTF-8 behaviour with".into(),
        });
    };
    // SAFETY: a valid glibc locale_t; the previous one is restored below.
    let old = unsafe { host_uselocale(l) };
    let r = f();
    // SAFETY: as above.
    unsafe { host_uselocale(old) };
    r
}

fn host_fn(name: &str) -> Option<usize> {
    let c = std::ffi::CString::new(name).ok()?;
    // SAFETY: a lookup in the host's global scope.
    let p = unsafe { host_dlsym(std::ptr::null_mut(), c.as_ptr()) };
    (!p.is_null()).then_some(p as usize)
}

/// The host's `host_name`, called under C.UTF-8 with the guest's first
/// `args.len()` arguments -- which drops a trailing `locale_t` that bionic
/// would have ignored anyway.
fn utf8_call(name: &'static str, host_name: &'static str, args: &'static [Ty], ret: Ret) -> Handler {
    let f = host_fn(host_name);
    Box::new(move |c| {
        count(name);
        let Some(f) = f else {
            return Err(Fault::Unsupported { thunk: name.into(), why: format!("the host has no {host_name}") });
        };
        under_utf8(name, || c.host(f as *const c_void, args, ret))
    })
}

thread_local! {
    /// bionic's per-thread `uselocale` slot; zero until first set, which
    /// reads as LC_GLOBAL_LOCALE.
    static CURRENT_LOCALE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn newlocale() -> Handler {
    Box::new(|c| {
        count("newlocale");
        let (mask, name) = (c.x(0) as u32 as u64, c.x(1) as *const c_char);
        if mask & !LC_ALL_MASK != 0 || name.is_null() {
            errno_set(EINVAL);
            c.set_x(0, 0);
            return Ok(());
        }
        // SAFETY: a non-null C string from the guest.
        let n = unsafe { CStr::from_ptr(name) }.to_bytes();
        if !matches!(n, b"" | b"C" | b"C.UTF-8" | b"en_US.UTF-8" | b"POSIX") {
            errno_set(ENOENT);
            c.set_x(0, 0);
            return Ok(());
        }
        let utf8 = n.is_empty() || n.windows(5).any(|w| w == b"UTF-8");
        // bionic's `struct __locale_t { size_t mb_cur_max; }`, owned by the
        // guest from here and freed by freelocale. bionic ignores `base`.
        let l = Box::into_raw(Box::new(if utf8 { 4u64 } else { 1 }));
        c.set_x(0, l as u64);
        Ok(())
    })
}

fn duplocale() -> Handler {
    Box::new(|c| {
        count("duplocale");
        let l = c.x(0);
        let v = if l == LC_GLOBAL_LOCALE {
            4
        } else {
            // SAFETY: a locale this layer made.
            unsafe { *(l as *const u64) }
        };
        c.set_x(0, Box::into_raw(Box::new(v)) as u64);
        Ok(())
    })
}

fn freelocale() -> Handler {
    Box::new(|c| {
        count("freelocale");
        let l = c.x(0);
        if l != 0 && l != LC_GLOBAL_LOCALE {
            // SAFETY: made by newlocale/duplocale above, freed once.
            drop(unsafe { Box::from_raw(l as *mut u64) });
        }
        Ok(())
    })
}

fn uselocale() -> Handler {
    Box::new(|c| {
        count("uselocale");
        let new = c.x(0);
        let old = CURRENT_LOCALE.with(|l| {
            let old = l.get();
            if new != 0 {
                l.set(new);
            }
            old
        });
        c.set_x(0, if old == 0 { LC_GLOBAL_LOCALE } else { old });
        Ok(())
    })
}

fn ctype_get_mb_cur_max() -> Handler {
    Box::new(|c| {
        count("__ctype_get_mb_cur_max");
        let l = CURRENT_LOCALE.with(|l| l.get());
        // Nothing here implements setlocale, so the global locale stays
        // bionic's default, which is UTF-8.
        let v = if l == 0 || l == LC_GLOBAL_LOCALE {
            4
        } else {
            // SAFETY: a locale this layer made.
            unsafe { *(l as *const u64) }
        };
        c.set_x(0, v);
        Ok(())
    })
}

/// bionic's `struct lconv`: POSIX's field order, ten pointers then fourteen
/// chars, as glibc's; the values are bionic's, "." and "" and CHAR_MAX.
#[repr(C)]
struct Lconv {
    strs: [*const c_char; 10],
    chars: [u8; 14],
}
struct LconvBox(Lconv);
// SAFETY: immutable after construction, pointing at static strings.
unsafe impl Sync for LconvBox {}
// SAFETY: as above.
unsafe impl Send for LconvBox {}

fn localeconv() -> Handler {
    static L: OnceLock<LconvBox> = OnceLock::new();
    Box::new(|c| {
        count("localeconv");
        let l = L.get_or_init(|| {
            let mut strs = [c"".as_ptr(); 10];
            strs[0] = c".".as_ptr();
            LconvBox(Lconv { strs, chars: [127; 14] })
        });
        c.set_x(0, &l.0 as *const Lconv as u64);
        Ok(())
    })
}

/// `strtold_l`: the guest's `long double` is an IEEE binary128 returned in
/// q0, the host's an x87 80-bit value. glibc parses into the 80-bit format
/// and the shim widens it, which is exact (the same exponent range, zero
/// bits appended) but carries only 64 significant bits where bionic's parser
/// keeps 113, so a value that needs more is rounded where bionic would not
/// have rounded it.
fn strtold() -> Handler {
    Box::new(|c| {
        count("strtold");
        let mut q = [0u64; 2];
        // SAFETY: the guest's string and end pointer.
        unsafe { cordial_guest::strtold_quad(c.x(0) as *const c_char, c.x(1) as *mut *mut c_char, &mut q) };
        c.set_v(0, q);
        Ok(())
    })
}

/// The multibyte, wide-character and `_l` functions: the host's, under
/// C.UTF-8, with any locale argument dropped as bionic drops it.
fn utf8_handler(name: &str) -> Option<(Handler, &'static str)> {
    use Ty::*;
    let (host, args, ret): (&'static str, &'static [Ty], Ret) = match name {
        "mbrtowc" => ("mbrtowc", &[Ptr, Ptr, U64, Ptr], Ret::Int(U64)),
        "mbrlen" => ("mbrlen", &[Ptr, U64, Ptr], Ret::Int(U64)),
        "mbtowc" => ("mbtowc", &[Ptr, Ptr, U64], Ret::Int(I32)),
        "mbsrtowcs" => ("mbsrtowcs", &[Ptr, Ptr, U64, Ptr], Ret::Int(U64)),
        "mbsnrtowcs" => ("mbsnrtowcs", &[Ptr, Ptr, U64, U64, Ptr], Ret::Int(U64)),
        "wcrtomb" => ("wcrtomb", &[Ptr, U32, Ptr], Ret::Int(U64)),
        "wcsnrtombs" => ("wcsnrtombs", &[Ptr, Ptr, U64, U64, Ptr], Ret::Int(U64)),
        "btowc" => ("btowc", &[I32], Ret::Int(U32)),
        "wctob" => ("wctob", &[U32], Ret::Int(I32)),
        "iswalpha_l" => ("iswalpha", &[U32], Ret::Int(I32)),
        "iswblank_l" => ("iswblank", &[U32], Ret::Int(I32)),
        "iswcntrl_l" => ("iswcntrl", &[U32], Ret::Int(I32)),
        "iswdigit_l" => ("iswdigit", &[U32], Ret::Int(I32)),
        "iswlower_l" => ("iswlower", &[U32], Ret::Int(I32)),
        "iswprint_l" => ("iswprint", &[U32], Ret::Int(I32)),
        "iswpunct_l" => ("iswpunct", &[U32], Ret::Int(I32)),
        "iswspace_l" => ("iswspace", &[U32], Ret::Int(I32)),
        "iswupper_l" => ("iswupper", &[U32], Ret::Int(I32)),
        "iswxdigit_l" => ("iswxdigit", &[U32], Ret::Int(I32)),
        "towlower_l" => ("towlower", &[U32], Ret::Int(U32)),
        "towupper_l" => ("towupper", &[U32], Ret::Int(U32)),
        "strtoll_l" => ("strtoll", &[Ptr, Ptr, I32], Ret::Int(I64)),
        "strtoull_l" => ("strtoull", &[Ptr, Ptr, I32], Ret::Int(U64)),
        "strcoll_l" => ("strcoll", &[Ptr, Ptr], Ret::Int(I32)),
        "strxfrm_l" => ("strxfrm", &[Ptr, Ptr, U64], Ret::Int(U64)),
        "wcscoll_l" => ("wcscoll", &[Ptr, Ptr], Ret::Int(I32)),
        "wcsxfrm_l" => ("wcsxfrm", &[Ptr, Ptr, U64], Ret::Int(U64)),
        "strftime_l" => ("strftime", &[Ptr, U64, Ptr, Ptr], Ret::Int(U64)),
        _ => return None,
    };
    let label: &'static str = Box::leak(name.to_owned().into_boxed_str());
    Some((utf8_call(label, host, args, ret), "the host's under C.UTF-8, locale argument dropped as bionic does"))
}

// ------------------------------------------------------ files: flags, stat
//
// The O_* bits that differ (design §3.1), arm64 against x86-64, from the two
// kernels' `asm/fcntl.h`: everything else is the generic value on both.
const O_PAIRS: [(u32, u32); 4] = [
    (0o040000, 0o0200000),  // O_DIRECTORY
    (0o100000, 0o0400000),  // O_NOFOLLOW
    (0o200000, 0o0040000),  // O_DIRECT
    (0o400000, 0o0100000),  // O_LARGEFILE
];
const O_DIFFERING_ARM64: u32 = 0o740000;
const O_DIFFERING_X86: u32 = 0o0740000;

fn oflags_to_host(f: u32) -> u32 {
    let mut out = f & !O_DIFFERING_ARM64;
    for (a, x) in O_PAIRS {
        if f & a != 0 {
            out |= x;
        }
    }
    out
}

fn oflags_to_guest(f: u32) -> u32 {
    let mut out = f & !O_DIFFERING_X86;
    for (a, x) in O_PAIRS {
        if f & x != 0 {
            out |= a;
        }
    }
    out
}

const O_CREAT: u32 = 0o100;
const O_TMPFILE_BIT: u32 = 0o20000000;

/// The native table's `open` (Cordial's `/system` redirect), kept when the
/// import is linked so the raw `openat` reaches the same path layer.
static NATIVE_OPEN: OnceLock<usize> = OnceLock::new();

const AT_FDCWD: i64 = -100;

/// `openat(dirfd, path, flags, mode)` with arm64's flags, shared by the
/// `open` imports and the raw syscall (`guest_sys::arm64_syscall`): the
/// CPU files from `cpu_file`, a path the process's working directory
/// resolves through the native `open`, and a path relative to a descriptor
/// the guest already opened through the host's `openat`, since the path
/// layer only ever rewrites absolute paths. The descriptor, or the errno.
pub(crate) fn guest_openat(dirfd: i64, path: u64, flags: u32, mode: u32) -> Result<Result<u64, c_int>, Fault> {
    openat_via("open", NATIVE_OPEN.get().copied(), dirfd, path, flags, mode)
}

/// `guest_openat` through the native function `name` (`open` or
/// `__open_2`, which are different functions in the native table).
pub(crate) fn openat_via(name: &'static str, native: Option<usize>, dirfd: i64, path: u64, flags: u32, mode: u32)
              -> Result<Result<u64, c_int>, Fault> {
    extern "C" {
        #[link_name = "openat"]
        fn host_openat(dirfd: c_int, path: *const c_char, flags: c_int, ...) -> c_int;
    }
    if let Some(fd) = cpu_file(path as *const c_char) {
        return Ok(if fd < 0 { Err(errno()) } else { Ok(fd as u64) });
    }
    let mode = if flags & (O_CREAT | O_TMPFILE_BIT) != 0 { mode } else { 0 };
    // SAFETY: the guest's path, when not null.
    let absolute = path != 0 && unsafe { *(path as *const u8) } == b'/';
    let fd = match native {
        None if absolute || dirfd == AT_FDCWD => {
            return Err(Fault::Unsupported { thunk: name.into(), why: "no open on the native path".into() });
        }
        Some(f) if absolute || dirfd == AT_FDCWD => {
            // SAFETY: the native function, called with the types its C
            // declaration gives; the path is the guest's, a host address.
            let out = unsafe { cordial_guest::invoke(name, f as *const c_void, &[Ty::Ptr, Ty::I32, Ty::U32],
                                                     &[path, oflags_to_host(flags) as u64, mode as u64]) }?;
            out.rax as u32 as i32
        }
        // SAFETY: the guest's descriptor and path.
        _ => unsafe { host_openat(dirfd as c_int, path as *const c_char, oflags_to_host(flags) as c_int, mode) },
    };
    Ok(if fd < 0 { Err(errno()) } else { Ok(fd as u64) })
}

/// `open(path, flags, ...)` and `__open_2(path, flags)`: `guest_openat` from
/// the working directory. The mode is the third register when the flags ask
/// for one, since AAPCS64 on Linux passes variadic arguments like named ones.
fn open(name: &'static str, native: Option<usize>) -> Handler {
    if let (Some(f), "open") = (native, name) {
        let _ = NATIVE_OPEN.set(f);
    }
    Box::new(move |c| {
        count(name);
        let mode = if name == "open" { c.x(2) as u32 } else { 0 };
        libc_ret(c, openat_via(name, native, AT_FDCWD, c.x(0), c.x(1) as u32, mode)?);
        Ok(())
    })
}

const F_GETFL: u64 = 3;
const F_SETFL: u64 = 4;

/// `fcntl(fd, cmd, ...)`. The command numbers and `struct flock` are the
/// generic ones on both kernels (arm64 has no separate `*64` commands, and
/// x86-64's are the same numbers); only the file status flags need
/// renumbering, in both directions.
fn fcntl() -> Handler {
    extern "C" {
        #[link_name = "fcntl"]
        fn host_fcntl(fd: c_int, cmd: c_int, arg: u64) -> c_int;
    }
    Box::new(|c| {
        count("fcntl");
        let (fd, cmd, arg) = (c.x(0) as c_int, c.x(1) as u32 as u64, c.x(2));
        let arg = if cmd == F_SETFL { oflags_to_host(arg as u32) as u64 } else { arg };
        // SAFETY: the guest's descriptor and argument; a pointer argument
        // (F_GETLK and friends) points at a layout both kernels share.
        let r = unsafe { host_fcntl(fd, cmd as c_int, arg) };
        let r = if cmd == F_GETFL && r >= 0 { oflags_to_guest(r as u32) as c_int } else { r };
        c.set_x(0, r as i64 as u64);
        Ok(())
    })
}

/// glibc x86-64 `struct stat` (144 bytes) into arm64's asm-generic one (128).
///
/// # Safety
/// `h` is a filled host stat; `g` points at 128 writable bytes.
unsafe fn stat_to_guest(h: &[u8; 144], g: *mut u8) {
    let rd = |o: usize| u64::from_ne_bytes(h[o..o + 8].try_into().unwrap());
    let rd4 = |o: usize| u32::from_ne_bytes(h[o..o + 4].try_into().unwrap());
    let mut out = [0u8; 128];
    let mut put = |o: usize, b: &[u8]| out[o..o + b.len()].copy_from_slice(b);
    put(0, &rd(0).to_ne_bytes()); // st_dev
    put(8, &rd(8).to_ne_bytes()); // st_ino
    put(16, &rd4(24).to_ne_bytes()); // st_mode
    put(20, &(rd(16) as u32).to_ne_bytes()); // st_nlink
    put(24, &rd4(28).to_ne_bytes()); // st_uid
    put(28, &rd4(32).to_ne_bytes()); // st_gid
    put(32, &rd(40).to_ne_bytes()); // st_rdev
    put(48, &rd(48).to_ne_bytes()); // st_size
    put(56, &(rd(56) as u32).to_ne_bytes()); // st_blksize
    put(64, &rd(64).to_ne_bytes()); // st_blocks
    for i in 0..6 {
        put(72 + 8 * i, &rd(72 + 8 * i).to_ne_bytes()); // atim, mtim, ctim
    }
    // SAFETY: the caller's guarantee.
    unsafe { std::ptr::copy_nonoverlapping(out.as_ptr(), g, 128) };
}

/// `stat`/`lstat` (path) and `fstat` (descriptor): the native answer fills a
/// host `struct stat`, which is then laid out as arm64's.
fn stat(name: &'static str, native: Option<usize>) -> Handler {
    Box::new(move |c| {
        count(name);
        let Some(f) = native else {
            return Err(Fault::Unsupported { thunk: name.into(), why: "no implementation on the native path".into() });
        };
        let mut h = [0u8; 144];
        let first = if name == "fstat" { Ty::I32 } else { Ty::Ptr };
        // SAFETY: the host function named by `name`, called with the types its
        // C declaration gives; pointers are the guest's, which are host addresses.
        let out = unsafe { cordial_guest::invoke(name, f as *const c_void, &[first, Ty::Ptr],
                                        &[c.x(0), h.as_mut_ptr() as u64]) }?;
        let r = out.rax as u32 as i32;
        if r == 0 {
            // SAFETY: the guest's struct stat.
            unsafe { stat_to_guest(&h, c.x(1) as *mut u8) };
        }
        c.set_x(0, r as i64 as u64);
        Ok(())
    })
}

/// `prctl(option, ...)`, passed through with all four further arguments.
/// Option numbers are one global list, not per architecture, and the
/// arm64-only ones (SVE vector length, PAC keys, tagged addresses, SME) get
/// EINVAL from an x86-64 kernel -- which is the true answer for the CPU the
/// guest runs on, since the translator has none of those features either.
fn prctl() -> Handler {
    extern "C" {
        #[link_name = "prctl"]
        fn host_prctl(o: c_int, ...) -> c_int;
    }
    Box::new(|c| {
        count("prctl");
        // SAFETY: the guest's own arguments; pointer arguments (PR_GET_NAME's
        // 16-byte buffer) have the same meaning on both kernels.
        let r = unsafe { host_prctl(c.x(0) as c_int, c.x(1), c.x(2), c.x(3), c.x(4)) };
        c.set_x(0, r as i64 as u64);
        Ok(())
    })
}

// ------------------------------------------------------------------ memory
//
// `mmap`, `munmap`, `mprotect` and `madvise` are the host's, through
// `cordial_guest::code`, which keeps the ranges the guest made executable
// and drops every Jit's translations of one the guest changes.

fn errno() -> c_int {
    extern "C" {
        fn __errno_location() -> *mut c_int;
    }
    // SAFETY: the calling thread's own errno.
    unsafe { *__errno_location() }
}

/// The answer to a call with C's failure convention, for a libc thunk: the
/// value, or -1 with errno set.
fn libc_ret(c: &Call, r: Result<u64, c_int>) {
    match r {
        Ok(v) => c.set_x(0, v),
        Err(e) => {
            errno_set(e);
            c.set_x(0, u64::MAX);
        }
    }
}

fn mmap() -> Handler {
    Box::new(|c| {
        count("mmap");
        let r = cordial_guest::code::mmap(c.x(0), c.x(1), c.x(2), c.x(3), c.x(4), c.x(5), c.x(30))?;
        libc_ret(c, r);
        Ok(())
    })
}

fn munmap() -> Handler {
    Box::new(|c| {
        count("munmap");
        libc_ret(c, cordial_guest::code::munmap(c.x(0), c.x(1), c.x(30))?);
        Ok(())
    })
}

fn mprotect() -> Handler {
    Box::new(|c| {
        count("mprotect");
        libc_ret(c, cordial_guest::code::mprotect(c.x(0), c.x(1), c.x(2), c.x(30))?);
        Ok(())
    })
}

fn madvise() -> Handler {
    Box::new(|c| {
        count("madvise");
        libc_ret(c, cordial_guest::code::madvise(c.x(0), c.x(1), c.x(2) as c_int, c.x(30))?);
        Ok(())
    })
}

// --------------------------------------------------------- va_list printf
//
// The `v` printf family takes an AAPCS64 `va_list`, a 32-byte structure
// pointing into the caller's register save area and stack, which the host's
// 24-byte SysV `va_list` cannot stand in for (design §3.1). The format says
// what each argument is; each is fetched from the guest's list as the
// callee's `va_arg` would, and the host's plain variadic function is called
// with them. What formats differently between glibc and bionic (`%p` of
// null) is a libc difference, as `thunks::printf_like` notes, not an ABI one.

/// Reads the arguments `fmt` consumes out of the guest `va_list` at `ap`.
fn va_values(name: &str, fmt: u64, ap: u64) -> Result<(Vec<Ty>, Vec<u64>), Fault> {
    // SAFETY: the guest's format string.
    let f = unsafe { CStr::from_ptr(fmt as *const c_char) };
    let types = cordial_guest::printf_types(f.to_bytes())
        .map_err(|why| Fault::Unsupported { thunk: name.into(), why })?;
    // SAFETY: by AAPCS64 a va_list argument is a pointer to the caller's.
    let mut va = unsafe { cordial_guest::VaList::read(ap) };
    let vals = types.iter().map(|&t| va.next(t)).collect();
    Ok((types, vals))
}

pub(crate) fn fortify_fatal(name: &str, what: &str) -> Fault {
    // bionic's __fortify_fatal: the message, then abort.
    eprintln!("FORTIFY: {name}: {what}");
    Fault::Exit { function: name.into(), status: 134 }
}

/// `vsnprintf(buf, n, fmt, ap)`, `__vsnprintf_chk(buf, n, flag, slen, fmt,
/// ap)`, `__vsprintf_chk(buf, flag, slen, fmt, ap)`, `vfprintf(fp, fmt,
/// ap)` and `vasprintf(out, fmt, ap)`, each onto the host's non-`v` twin.
fn vprintf(name: &'static str, native: &dyn Fn(&str) -> Option<usize>) -> Handler {
    let twin = match name {
        "vfprintf" => "fprintf",
        "vasprintf" => "asprintf",
        _ => "snprintf",
    };
    // The native answer where there is one (Cordial's FILE functions know
    // its legacy __sF streams); glibc's otherwise.
    let f = native(twin).or_else(|| host_fn(twin));
    Box::new(move |c| {
        count(name);
        let Some(f) = f else {
            return Err(Fault::Unsupported { thunk: name.into(), why: format!("no host {twin}") });
        };
        // (named arguments passed on, index of fmt, index of the va_list)
        let (mut types, mut vals, fmt, ap): (Vec<Ty>, Vec<u64>, u32, u32) = match name {
            "vsnprintf" => (vec![Ty::Ptr, Ty::U64], vec![c.x(0), c.x(1)], 2, 3),
            "__vsnprintf_chk" => {
                if c.x(1) > c.x(3) {
                    return Err(fortify_fatal(name, "prevented write past end of buffer"));
                }
                (vec![Ty::Ptr, Ty::U64], vec![c.x(0), c.x(1)], 4, 5)
            }
            "__vsprintf_chk" => (vec![Ty::Ptr, Ty::U64], vec![c.x(0), c.x(2)], 3, 4),
            "vfprintf" | "vasprintf" => (vec![Ty::Ptr], vec![c.x(0)], 1, 2),
            _ => unreachable!(),
        };
        let (t, v) = va_values(name, c.x(fmt), c.x(ap))?;
        types.push(Ty::Ptr);
        vals.push(c.x(fmt));
        types.extend(t);
        vals.extend(v);
        // SAFETY: the host function named by `name`, called with the types its
        // C declaration gives; pointers are the guest's, which are host addresses.
        let out = unsafe { cordial_guest::invoke(name, f as *const c_void, &types, &vals) }?;
        let r = out.rax as u32 as i32;
        if name == "__vsprintf_chk" && r >= 0 && r as u64 >= c.x(2) {
            return Err(fortify_fatal(name, "prevented write past end of buffer"));
        }
        c.set_x(0, r as i64 as u64);
        Ok(())
    })
}

// ----------------------------------------------------------------- syscall
//
// `syscall(nr, ...)`, arm64's numbering: the same translation the guest's own
// `svc #0` gets (`guest_sys::arm64_syscall`), with C's -1 and errno.

const ENOSYS: c_int = 38;

fn syscall() -> Handler {
    Box::new(|c| {
        count("syscall");
        let nr = c.x(0);
        let args = [c.x(1), c.x(2), c.x(3), c.x(4), c.x(5), c.x(6)];
        let r = match crate::guest_sys::arm64_syscall(nr, args, c.x(30))? {
            Some(r) => r,
            None => {
                crate::guest_sys::name_untranslated("syscall()", nr, &args);
                -(ENOSYS as i64)
            }
        };
        libc_ret(c, if (-4095..0).contains(&r) { Err(-r as c_int) } else { Ok(r as u64) });
        Ok(())
    })
}

// ------------------------------------------------------------------- scanf
//
// Every argument a scanf conversion consumes is a pointer, so the format only
// has to say how many there are. The pointed-to sizes agree between arm64
// bionic and x86-64 glibc for every length modifier (both LP64) except `L`
// with a floating conversion, which stores a `long double`: binary128 in the
// guest, 80-bit in the host. That is refused, as is `%m`, whose buffer would
// come from the host's malloc for a guest that frees through its own.

/// How many pointer arguments `fmt` consumes.
fn scanf_arg_count(fmt: &[u8]) -> Result<usize, String> {
    let mut n = 0;
    let mut i = 0;
    while i < fmt.len() {
        if fmt[i] != b'%' {
            i += 1;
            continue;
        }
        i += 1;
        if fmt.get(i) == Some(&b'%') {
            i += 1;
            continue;
        }
        let suppress = fmt.get(i) == Some(&b'*');
        if suppress {
            i += 1;
        }
        while i < fmt.len() && fmt[i].is_ascii_digit() {
            i += 1;
        }
        if fmt.get(i) == Some(&b'$') {
            return Err("positional scanf arguments are not supported".into());
        }
        if fmt.get(i) == Some(&b'm') {
            return Err("%m allocates with the host's malloc".into());
        }
        let mut long_double = false;
        while i < fmt.len() && b"hljztqL".contains(&fmt[i]) {
            long_double |= fmt[i] == b'L';
            i += 1;
        }
        let Some(&conv) = fmt.get(i) else {
            return Err("format ends inside a conversion".into());
        };
        i += 1;
        match conv {
            b'[' => {
                // A scanset: `]` straight after `[` or `[^` is literal.
                if fmt.get(i) == Some(&b'^') {
                    i += 1;
                }
                if fmt.get(i) == Some(&b']') {
                    i += 1;
                }
                while i < fmt.len() && fmt[i] != b']' {
                    i += 1;
                }
                i += 1;
            }
            b'e' | b'E' | b'f' | b'F' | b'g' | b'G' | b'a' | b'A' if long_double => {
                return Err("%L: the guest's long double is binary128, the host's 80-bit".into());
            }
            b'd' | b'i' | b'u' | b'o' | b'x' | b'X' | b'c' | b's' | b'p' | b'n' | b'e' | b'E' | b'f'
            | b'F' | b'g' | b'G' | b'a' | b'A' => {}
            c => return Err(format!("unknown scanf conversion %{}", c as char)),
        }
        if !suppress {
            n += 1;
        }
    }
    Ok(n)
}

/// `sscanf(str, fmt, ...)`, `fscanf(fp, fmt, ...)` and `vsscanf(str, fmt,
/// ap)`, onto the host's `sscanf`/`fscanf` (the native answer for `fscanf`
/// where there is one, which knows Cordial's legacy streams).
fn scanf(name: &'static str, native: &dyn Fn(&str) -> Option<usize>) -> Handler {
    let twin = if name == "fscanf" { "fscanf" } else { "sscanf" };
    let f = native(twin).or_else(|| host_fn(twin));
    Box::new(move |c| {
        count(name);
        let Some(f) = f else {
            return Err(Fault::Unsupported { thunk: name.into(), why: format!("no host {twin}") });
        };
        // SAFETY: the guest's format string.
        let fmt = unsafe { CStr::from_ptr(c.x(1) as *const c_char) };
        let n = scanf_arg_count(fmt.to_bytes()).map_err(|why| Fault::Unsupported { thunk: name.into(), why })?;
        let mut types = vec![Ty::Ptr, Ty::Ptr];
        let mut vals = vec![c.x(0), c.x(1)];
        if name == "vsscanf" {
            // SAFETY: by AAPCS64 a va_list argument points at the caller's.
            let mut va = unsafe { cordial_guest::VaList::read(c.x(2)) };
            for _ in 0..n {
                types.push(Ty::Ptr);
                vals.push(va.next(Ty::Ptr));
            }
        } else {
            let rest = cordial_guest::collect(c, &[&[Ty::Ptr, Ty::Ptr][..], &vec![Ty::Ptr; n]].concat());
            types.extend(vec![Ty::Ptr; n]);
            vals.extend_from_slice(&rest[2..]);
        }
        // SAFETY: the host function named by `name`, called with the types its
        // C declaration gives; pointers are the guest's, which are host addresses.
        let out = unsafe { cordial_guest::invoke(name, f as *const c_void, &types, &vals) }?;
        c.set_x(0, out.rax as u32 as i32 as i64 as u64);
        Ok(())
    })
}

// ------------------------------------------------- the CPU, seen from files
//
// `/proc/cpuinfo` and `/proc/self/auxv` describe the CPU the same way
// `getauxval` does, so a guest that reads the file gets the answer the call
// gave it (design §1.3) rather than the host's x86-64 flags. The engine does
// read cpuinfo during its constructors (64 `fscanf`s at M3); qemu-user, the
// Phase 1 reference, fakes an arm64 cpuinfo too, with its own CPU's features.
// Each open gets a fresh memfd holding the text, so reads, seeks and
// `fdopen` behave as on a real file.

/// The kernel's `/proc/cpuinfo` names for AT_HWCAP bits 0..21
/// (`arch/arm64/kernel/cpuinfo.c`, `hwcap_str`).
const HWCAP_NAMES: [&str; 22] = [
    "fp", "asimd", "evtstrm", "aes", "pmull", "sha1", "sha2", "crc32", "atomics", "fphp", "asimdhp", "cpuid",
    "asimdrdm", "jscvt", "fcma", "lrcpc", "dcpop", "sha3", "sm3", "sm4", "asimddp", "sha512",
];

fn cpuinfo_text() -> String {
    let hw = guest_hwcap();
    let features: Vec<&str> =
        HWCAP_NAMES.iter().enumerate().filter(|(i, _)| hw & (1 << i) != 0).map(|(_, n)| *n).collect();
    let n = std::thread::available_parallelism().map_or(1, |n| n.get());
    let mut s = String::new();
    for i in 0..n {
        // Implementer 0x00 is "reserved for software use" (design §1.3):
        // no real core is claimed, and the part number says nothing either.
        s += &format!(
            "processor\t: {i}\nFeatures\t: {}\nCPU implementer\t: 0x00\nCPU architecture: 8\n\
             CPU variant\t: 0x0\nCPU part\t: 0x000\nCPU revision\t: 0\n\n",
            features.join(" ")
        );
    }
    s
}

/// The `auxv` entries `getauxval` answers, as the kernel lays them out:
/// (type, value) pairs of u64, ending with AT_NULL.
fn auxv_bytes() -> Vec<u8> {
    extern "C" {
        #[link_name = "getauxval"]
        fn host_getauxval(t: u64) -> u64;
    }
    let mut v: Vec<(u64, u64)> = vec![(AT_HWCAP, guest_hwcap()), (AT_HWCAP2, 0), (AT_PAGESZ, 4096),
                                      (AT_CLKTCK, 100), (AT_PLATFORM, c"aarch64".as_ptr() as u64)];
    for t in [AT_UID, AT_EUID, AT_GID, AT_EGID, AT_SECURE, AT_RANDOM] {
        // SAFETY: plain libc call.
        v.push((t, unsafe { host_getauxval(t) }));
    }
    v.push((0, 0));
    v.iter().flat_map(|(t, x)| t.to_ne_bytes().into_iter().chain(x.to_ne_bytes())).collect()
}

/// A descriptor on an in-memory copy of the CPU file `path` names, or None
/// for any other path.
fn cpu_file(path: *const c_char) -> Option<c_int> {
    extern "C" {
        fn memfd_create(name: *const c_char, flags: u32) -> c_int;
        #[link_name = "write"]
        fn host_write(fd: c_int, p: *const c_void, n: usize) -> isize;
        #[link_name = "lseek"]
        fn host_lseek(fd: c_int, off: i64, whence: c_int) -> i64;
    }
    if path.is_null() {
        return None;
    }
    // SAFETY: the guest's path string.
    let bytes = match unsafe { CStr::from_ptr(path) }.to_bytes() {
        b"/proc/cpuinfo" => cpuinfo_text().into_bytes(),
        b"/proc/self/auxv" => auxv_bytes(),
        _ => return None,
    };
    const MFD_CLOEXEC: u32 = 1;
    // SAFETY: plain libc calls on a descriptor this function owns.
    unsafe {
        let fd = memfd_create(c"cordial-guest-cpu".as_ptr(), MFD_CLOEXEC);
        if fd < 0 {
            return Some(fd);
        }
        host_write(fd, bytes.as_ptr().cast(), bytes.len());
        host_lseek(fd, 0, 0);
        Some(fd)
    }
}

/// `fopen`: the CPU files from memory, everything else through the native
/// answer (Cordial's `/system` redirect).
fn fopen(native: Option<usize>) -> Handler {
    extern "C" {
        fn fdopen(fd: c_int, mode: *const c_char) -> *mut c_void;
    }
    Box::new(move |c| {
        count("fopen");
        if let Some(fd) = cpu_file(c.x(0) as *const c_char) {
            // SAFETY: our descriptor and the guest's mode string.
            let f = if fd < 0 { std::ptr::null_mut() } else { unsafe { fdopen(fd, c.x(1) as *const c_char) } };
            c.set_x(0, f as u64);
            return Ok(());
        }
        let Some(f) = native else {
            return Err(Fault::Unsupported { thunk: "fopen".into(), why: "no fopen on the native path".into() });
        };
        c.host(f as *const c_void, &[Ty::Ptr, Ty::Ptr], Ret::Int(Ty::Ptr))
    })
}

/// The hand-written thunk for `name`, if there is one, and a note for the
/// import table.
/// `native` is what the x86-64 path would call for the same name, where a
/// thunk translates around it rather than replacing it.
pub fn handler(name: &str, rt: &Arc<Runtime>, native_of: &dyn Fn(&str) -> Option<usize>)
    -> Option<(Handler, &'static str)>
{
    let native = native_of(name);
    Some(match name {
        "getauxval" => (getauxval(), "the emulated CPU's hwcaps (design §1.3)"),
        "uname" => (uname(), "the host's, with machine = aarch64"),
        "__cxa_atexit" => (cxa_atexit(), "recorded; run by __cxa_finalize"),
        "__cxa_finalize" => (cxa_finalize(), "runs the recorded guest handlers"),
        "__cxa_thread_atexit_impl" => (cxa_thread_atexit_impl(), "recorded per thread for guest thread exit"),
        "pthread_once" => (pthread_once(), "bionic's once states, guest initialiser"),
        "pthread_key_create" if guest_keys() => (guest_key_create(), "bionic's key map (cordial-guest keys.rs)"),
        "pthread_key_delete" if guest_keys() => (guest_key_delete(), "bionic's key map (cordial-guest keys.rs)"),
        "pthread_key_create" => (pthread_key_create(), "host key, guest destructor recorded"),
        "bsearch" => (bsearch(), "binary search calling the guest comparator"),
        "dlopen" => (dlopen(), "the bionic linker, with the guest caller's address"),
        "dlsym" => (dlsym(), "the bionic linker, refusing host addresses"),
        "dlerror" => (dlerror(), "the bionic linker's message"),
        "dladdr" => (dladdr(), "the bionic linker"),
        "dlclose" => (dlclose(), "the bionic linker"),
        "dl_iterate_phdr" => (dl_iterate_phdr(), "guest objects only, with their own phdrs"),
        "newlocale" => (newlocale(), "bionic's locale model"),
        "duplocale" => (duplocale(), "bionic's locale model"),
        "freelocale" => (freelocale(), "bionic's locale model"),
        "uselocale" => (uselocale(), "bionic's locale model"),
        "__ctype_get_mb_cur_max" => (ctype_get_mb_cur_max(), "bionic's locale model"),
        "localeconv" => (localeconv(), "bionic's C lconv"),
        "strtold_l" => (strtold(), "glibc's parse, widened to binary128"),
        "open" => (open("open", native), "the native open, O_* renumbered for arm64"),
        "__open_2" => (open("__open_2", native), "the native open, O_* renumbered for arm64"),
        "fcntl" => (fcntl(), "the host's, file status flags renumbered for arm64"),
        "fopen" => (fopen(native), "the native fopen; /proc/cpuinfo and /proc/self/auxv describe the emulated CPU"),
        "prctl" => (prctl(), "the host's; arm64-only options fail as the CPU lacks them"),
        "sscanf" => (scanf("sscanf", native_of), "the host's, arguments counted from the format"),
        "fscanf" => (scanf("fscanf", native_of), "the host's, arguments counted from the format"),
        "vsscanf" => (scanf("vsscanf", native_of), "the guest va_list walked by format"),
        "syscall" => (syscall(), "arm64 numbers mapped for arch-neutral calls, ENOSYS otherwise"),
        "mmap" => (mmap(), "the host's; executable guest regions tracked"),
        "vsnprintf" => (vprintf("vsnprintf", native_of), "the guest va_list walked by format"),
        "__vsnprintf_chk" => (vprintf("__vsnprintf_chk", native_of), "the guest va_list walked by format"),
        "__vsprintf_chk" => (vprintf("__vsprintf_chk", native_of), "the guest va_list walked by format"),
        "vfprintf" => (vprintf("vfprintf", native_of), "the guest va_list walked by format"),
        "vasprintf" => (vprintf("vasprintf", native_of), "the guest va_list walked by format"),
        "munmap" => (munmap(), "the host's; translations of executable guest code dropped in every Jit"),
        "mprotect" => (mprotect(), "the host's; translations of executable guest code dropped in every Jit"),
        "madvise" => (madvise(), "the host's; discarding executable guest code drops its translations"),
        "stat" => (stat("stat", native), "the native stat, laid out as arm64's struct stat"),
        "lstat" => (stat("lstat", native), "the native lstat, laid out as arm64's struct stat"),
        "fstat" => (stat("fstat", native), "the native fstat, laid out as arm64's struct stat"),
        _ => return crate::guest_sys::handler(name, rt, native_of).or_else(|| utf8_handler(name)),
    })
}


#[cfg(test)]
mod tests {
    use super::*;

    /// The honest AT_HWCAP names none of the features the translator lacks.
    #[test]
    fn hwcap_omits_what_the_translator_lacks() {
        // ATOMICS, FPHP, ASIMDHP, CPUID, ASIMDRDM, JSCVT, LRCPC, SVE.
        for bit in [8, 9, 10, 11, 12, 13, 15, 22] {
            if bit == 8 && std::env::var_os("CORDIAL_GUEST_HWCAP_ATOMICS").is_some() {
                continue;
            }
            assert_eq!(guest_hwcap() & (1 << bit), 0, "bit {bit}");
        }
        assert_eq!(guest_hwcap() & 3, 3, "FP and ASIMD");
    }

    /// Every O_* bit survives arm64 -> host -> arm64, and the four that
    /// differ land on x86-64's numbers.
    #[test]
    fn oflags_round_trip_and_renumber() {
        for bit in 0..32 {
            let f = 1u32 << bit;
            assert_eq!(oflags_to_guest(oflags_to_host(f)), f, "bit {bit}");
        }
        assert_eq!(oflags_to_host(0o040000), 0o200000, "O_DIRECTORY");
        assert_eq!(oflags_to_host(0o100000 | 0o2000000 | 2), 0o400000 | 0o2000000 | 2, "O_NOFOLLOW|O_CLOEXEC|O_RDWR");
        assert_eq!(oflags_to_guest(0o100000), 0o400000, "O_LARGEFILE back");
    }

    /// arm64's struct stat, from a real host stat of this file.
    #[test]
    fn stat_is_laid_out_as_arm64s() {
        extern "C" {
            #[link_name = "stat"]
            fn host_stat(p: *const c_char, out: *mut u8) -> c_int;
        }
        let mut h = [0u8; 144];
        // SAFETY: a C string and a buffer larger than glibc's struct stat.
        assert_eq!(unsafe { host_stat(c"/proc/self/exe".as_ptr(), h.as_mut_ptr()) }, 0);
        let mut g = [0xaau8; 128];
        // SAFETY: both buffers are the sizes the function expects.
        unsafe { stat_to_guest(&h, g.as_mut_ptr()) };
        let q = |b: &[u8], o: usize| u64::from_ne_bytes(b[o..o + 8].try_into().unwrap());
        let d = |b: &[u8], o: usize| u32::from_ne_bytes(b[o..o + 4].try_into().unwrap());
        assert_eq!(q(&g, 8), q(&h, 8), "st_ino");
        assert_eq!(d(&g, 16), d(&h, 24), "st_mode");
        assert_eq!(q(&g, 48), q(&h, 48), "st_size");
        assert_eq!(q(&g, 64), q(&h, 64), "st_blocks");
        assert_eq!(q(&g, 88), q(&h, 88), "st_mtime");
        assert_eq!(&g[40..48], &[0; 8], "padding cleared");
    }

    #[test]
    fn scanf_counts_pointers_and_refuses_what_differs() {
        assert_eq!(scanf_arg_count(b"%d %*s %lf %[^]x] %n%%").unwrap(), 4);
        assert_eq!(scanf_arg_count(b"cpu MHz : %f").unwrap(), 1);
        assert!(scanf_arg_count(b"%Lf").is_err());
        assert!(scanf_arg_count(b"%ms").is_err());
    }

    /// The cpuinfo the guest reads names exactly the hwcaps getauxval gives.
    #[test]
    fn cpuinfo_matches_hwcap() {
        let t = cpuinfo_text();
        let line = t.lines().find(|l| l.starts_with("Features")).unwrap();
        let names: Vec<&str> = line.split(':').nth(1).unwrap().split_whitespace().collect();
        for (i, n) in HWCAP_NAMES.iter().enumerate() {
            assert_eq!(names.contains(n), guest_hwcap() & (1 << i) != 0, "{n}");
        }
        assert!(t.contains("CPU implementer\t: 0x00"));
    }
}
