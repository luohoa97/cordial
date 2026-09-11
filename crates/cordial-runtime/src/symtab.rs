//! Builds the `{soname -> {symbol -> address}}` tables handed to the linker.
//!
//! Each of Roblox's imports resolves one of two ways:
//!
//! * **host** — the desktop already has a compatible implementation. True for
//!   libm and libz (plain C, scalar and pointer arguments) and for GLES2/EGL
//!   (Khronos-specified; Mesa implements the same contract).
//! * **stub** — everything else, for now.
//!
//! `libc` is deliberately *not* resolved from the host by default. bionic and
//! glibc disagree on `struct stat`, `pthread_mutex_t`, `DIR`, `FILE` and
//! `sigset_t`, so passthrough would silently corrupt rather than work. Closing
//! that gap is what a bionic shim is for; see docs/base-evaluation.md §4.
//!
//! That is a default for the *library*, not a verdict on every symbol in it. An
//! individual entry point whose arguments are laid out identically in both
//! libcs can be answered from the host safely, and five of them are: see
//! `bionic::pthread`'s `once` for the ABI comparison that has to be done, per
//! symbol and measured, before adding a sixth.

use std::collections::BTreeMap;
use std::ffi::{c_char, c_int, c_void, CString};

use crate::stubs::SYMBOLS;

/// Symbol prefix -> the Android library that provides it. These have no host
/// equivalent, so they are always stubbed; the mapping only decides which
/// soname Cordial registers them under.
const ANDROID_PREFIXES: &[(&str, &str)] = &[
    ("AMedia", "libmediandk.so"),
    ("AMEDIA", "libmediandk.so"),
    ("AImage", "libmediandk.so"),
    ("AIMAGE", "libmediandk.so"),
    ("AndroidBitmap", "libjnigraphics.so"),
    ("__android_log", "liblog.so"),
    ("android_set_abort_message", "liblog.so"),
    ("android_get_device_api_level", "liblog.so"),
    ("ANative", "libandroid.so"),
    ("AAsset", "libandroid.so"),
    ("AInput", "libandroid.so"),
    ("AKey", "libandroid.so"),
    ("AMotion", "libandroid.so"),
    ("ALooper", "libandroid.so"),
    ("ASensor", "libandroid.so"),
    ("AChoreographer", "libandroid.so"),
    ("AConfiguration", "libandroid.so"),
    ("ATrace", "libandroid.so"),
    ("AHardwareBuffer", "libandroid.so"),
    ("ASharedMemory", "libandroid.so"),
    ("APerformanceHint", "libandroid.so"),
    ("AObb", "libandroid.so"),
    ("AStorageManager", "libandroid.so"),
    ("ASurface", "libandroid.so"),
    ("AFont", "libandroid.so"),
    ("ASystemFont", "libandroid.so"),
    // OpenSL ES. Current Roblox builds reference these directly rather than
    // through `dlsym`, and seven of the eight are data symbols, so they must
    // resolve at load time or `libroblox.so` does not load at all.
    ("SL_IID_", "libOpenSLES.so"),
    ("slCreateEngine", "libOpenSLES.so"),
];

/// The soname FMOD's Android output looks for once
/// `org.fmod.FMOD.supportsAAudio()` has said yes. Measured, not inferred: a
/// run with `CORDIAL_TRACE_DLSYM=1` and `supportsAAudio()` answering true is
/// what shows the `dlopen` happening at all — with it answering false, as it
/// did before `native/aaudio.cpp` existed, the guest never asks for any audio
/// library by any name.
pub const AAUDIO_LIBRARY_NAME: &str = "libaaudio.so";

/// In `libroblox.so`'s `DT_NEEDED` but contributing no undefined symbols — they
/// are consulted via `dlsym` at runtime, if at all. They still have to exist for
/// the `DT_NEEDED` walk to succeed.
pub const EMPTY_LIBRARIES: &[&str] = &["libOpenMAXAL.so"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Implemented by Cordial itself (see `bionic`).
    Cordial,
    /// The host's own library, used directly.
    Host,
    /// Not implemented yet.
    Stub,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Cordial => "cordial",
            Source::Host => "host",
            Source::Stub => "stub",
        }
    }
}

pub struct Entry {
    pub symbol: &'static str,
    pub address: *mut c_void,
    pub source: Source,
}

#[derive(Default, Clone, Copy)]
pub struct Stats {
    pub cordial: usize,
    pub host: usize,
    pub stub: usize,
}

impl Stats {
    fn record(&mut self, source: Source) {
        match source {
            Source::Cordial => self.cordial += 1,
            Source::Host => self.host += 1,
            Source::Stub => self.stub += 1,
        }
    }
}

pub struct SymbolTable {
    pub libraries: BTreeMap<&'static str, Vec<Entry>>,
    pub stats: BTreeMap<&'static str, Stats>,
    /// Host libraries that could not be opened; their symbols fell back to stubs.
    pub missing_host_libs: Vec<&'static str>,
}

impl SymbolTable {
    pub fn totals(&self) -> Stats {
        self.stats.values().fold(Stats::default(), |mut acc, s| {
            acc.cordial += s.cordial;
            acc.host += s.host;
            acc.stub += s.stub;
            acc
        })
    }
}

/// How a symbol is classified before resolution is attempted.
enum Class {
    /// Android-only; no host implementation exists.
    Android(&'static str),
    /// Khronos API; try the host, fall back to a stub, but always register under
    /// the given soname so the bucketing stays honest either way.
    Khronos(&'static str),
    /// Anything else — libc, libm, libz. Whichever host library answers decides.
    Generic,
}

fn classify(symbol: &str) -> Class {
    // `gl`/`egl` followed by a capital, so `glob` and friends do not match.
    if let Some(rest) = symbol.strip_prefix("egl") {
        if rest.starts_with(char::is_uppercase) {
            return Class::Khronos("libEGL.so");
        }
    }
    if let Some(rest) = symbol.strip_prefix("gl") {
        if rest.starts_with(char::is_uppercase) {
            return Class::Khronos("libGLESv2.so");
        }
    }
    for (prefix, lib) in ANDROID_PREFIXES {
        if symbol.starts_with(prefix) {
            return Class::Android(lib);
        }
    }
    Class::Generic
}

/// Build the full table.
///
/// `host_libc` resolves libc symbols from the host as well. It is ABI-unsafe and
/// exists to see how far execution gets, not to be correct.
pub fn build(host_libc: bool) -> SymbolTable {
    // (host soname, Android soname it stands in for)
    //
    // libstdc++ and libgcc_s are here for one symbol: `__gxx_personality_v0`,
    // the Itanium C++ ABI personality routine. Roblox imports it undefined and
    // throws during static initialisation; with it stubbed the unwinder cannot
    // find a handler, calls std::terminate, and the whole load aborts inside
    // DT_INIT_ARRAY. The ABI is standard, so the host's is the right one.
    #[cfg(not(target_os = "freebsd"))]
    let candidates: &[(&'static str, &'static str)] = &[
        ("libm.so.6", "libm.so"),
        ("libz.so.1", "libz.so"),
        ("libGLESv2.so.2", "libGLESv2.so"),
        ("libEGL.so.1", "libEGL.so"),
        ("libstdc++.so.6", "libc.so"),
        ("libgcc_s.so.1", "libc.so"),
    ];
    // FreeBSD spells the libc-family sonames differently, and the Itanium C++
    // ABI personality (__gxx_personality_v0) lives in libcxxrt, not libstdc++.
    // GLESv2/EGL keep their sonames (mesa/nvidia use the same).
    #[cfg(target_os = "freebsd")]
    let candidates: &[(&'static str, &'static str)] = &[
        ("libm.so.5", "libm.so"),
        ("libz.so.6", "libz.so"),
        ("libGLESv2.so.2", "libGLESv2.so"),
        ("libEGL.so.1", "libEGL.so"),
        ("libcxxrt.so.1", "libc.so"),
        ("libgcc_s.so.1", "libc.so"),
    ];

    let overrides: BTreeMap<&'static str, *mut c_void> = crate::bionic::function_overrides()
        .into_iter()
        .chain(crate::bionic::data_overrides())
        .chain(crate::android::overrides())
        .collect();

    let mut host_libs = Vec::new();
    let mut missing_host_libs = Vec::new();
    for (soname, provides) in candidates {
        match HostLib::open(soname, provides) {
            Some(lib) => host_libs.push(lib),
            None => missing_host_libs.push(*soname),
        }
    }
    #[cfg(target_os = "freebsd")]
    let host_libc_soname = "libc.so.7";
    #[cfg(not(target_os = "freebsd"))]
    let host_libc_soname = "libc.so.6";
    // On FreeBSD the host libm does not re-export libc through dlsym the way
    // glibc's does, so the ABI-safe generics (memset, newlocale, mbtowc, …) have
    // nowhere to resolve from. Open the host libc unconditionally and let the
    // Generic fall-through below consult it, minus an ABI-unsafe denylist.
    #[cfg(target_os = "freebsd")]
    let libc = HostLib::open(host_libc_soname, "libc.so").or_else(|| {
        let _ = host_libc;
        None
    });
    #[cfg(not(target_os = "freebsd"))]
    let libc = host_libc
        .then(|| HostLib::open(host_libc_soname, "libc.so"))
        .flatten();

    let mut table = SymbolTable {
        libraries: BTreeMap::new(),
        stats: BTreeMap::new(),
        missing_host_libs,
    };

    for (symbol, stub) in SYMBOLS.iter() {
        let stub_addr = *stub as *mut c_void;
        let class = classify(symbol);

        // Cordial's own implementations win over everything. They exist because
        // neither the host nor a stub is correct for these; see `bionic`.
        let (library, address, source) = if let Some(&addr) = overrides.get(symbol) {
            let lib = match class {
                Class::Android(lib) | Class::Khronos(lib) => lib,
                Class::Generic => "libc.so",
            };
            (lib, addr, Source::Cordial)
        } else {
            match class {
                Class::Android(lib) => (lib, stub_addr, Source::Stub),

                Class::Khronos(lib) => match lookup(&host_libs, symbol) {
                    Some((_, addr)) => (lib, addr, Source::Host),
                    None => (lib, stub_addr, Source::Stub),
                },

                Class::Generic => match lookup(&host_libs, symbol) {
                    Some((provides, addr)) => (provides, addr, Source::Host),
                    None => {
                        // Fall through to the host libc, except where the host's
                        // ABI differs from bionic's: mutex/rwlock objects have a
                        // different size and layout, and syscall(2) numbers are
                        // not the same. Those stay stubbed until translated.
                        let hit = if abi_unsafe_generic(symbol) {
                            None
                        } else {
                            libc.as_ref().and_then(|l| l.lookup(symbol))
                        };
                        match hit {
                            Some(addr) => ("libc.so", addr, Source::Host),
                            None => ("libc.so", stub_addr, Source::Stub),
                        }
                    }
                },
            }
        };

        table.libraries.entry(library).or_default().push(Entry {
            symbol,
            address,
            source,
        });
        table.stats.entry(library).or_default().record(source);
    }

    for name in EMPTY_LIBRARIES {
        table.libraries.entry(name).or_default();
        table.stats.entry(name).or_default();
    }

    // Vulkan is `dlopen`ed, not linked — it contributes no undefined symbols to
    // `libroblox.so` and so never reaches the per-symbol classification above.
    // Register it as its own virtual library, exporting only
    // `vkGetInstanceProcAddr`; everything else is fetched dynamically through
    // it. See `android::vulkan`. When the host has no Vulkan at all, both
    // sonames are left unregistered and Roblox's `dlopen` fails exactly as it
    // does today — a clean fall-through to GLES.
    match crate::android::vulkan::get_instance_proc_addr_symbol() {
        Some(addr) => {
            for name in crate::android::vulkan::LIBRARY_NAMES {
                table.libraries.entry(name).or_default().push(Entry {
                    symbol: "vkGetInstanceProcAddr",
                    address: addr,
                    source: Source::Cordial,
                });
                table.stats.entry(name).or_default().record(Source::Cordial);
            }
        }
        None => table.missing_host_libs.push("libvulkan.so.1"),
    }

    // mimalloc, the same shape as Vulkan above: `libroblox.so` contributes no
    // undefined `mi_` symbols (nothing DT_NEEDS it), so it is never reached by
    // the per-symbol classification loop and has to be registered as its own
    // virtual library for whatever `dlopen`s it to find. See `mimalloc_lib`
    // for why this links the real allocator rather than stubbing its option
    // getters, and for what is and is not confirmed about whether the engine
    // ever asks for it.
    {
        let mimalloc = table
            .libraries
            .entry(crate::mimalloc_lib::LIBRARY_NAME)
            .or_default();
        for (symbol, address) in crate::mimalloc_lib::overrides() {
            mimalloc.push(Entry {
                symbol,
                address,
                source: Source::Cordial,
            });
            table
                .stats
                .entry(crate::mimalloc_lib::LIBRARY_NAME)
                .or_default()
                .record(Source::Cordial);
        }
    }

    // AAudio, and the same shape again: `libroblox.so` has no undefined
    // `AAudio*` symbols, so nothing here is reached by the per-symbol loop
    // above and the library has to be registered whole.
    //
    // **Registered unless `CORDIAL_AUDIO=java` asked otherwise.** This was off
    // by default while nothing had been measured — an audio backend nobody has
    // numbers for must not arrive with an update and take everyone's sound
    // with it — and the numbers now exist in
    // `docs/analysis/aaudio-contract.md`. Registering the library is still not
    // on its own enough to route anything: `org.fmod.FMOD.supportsAAudio()`
    // is the gate FMOD actually asks, and it answers false on a host with no
    // PipeWire session whatever this does. The condition is read out of
    // `native/aaudio.cpp` rather than from the environment here so that this
    // and that predicate cannot disagree — see `bionic::aaudio_selected`.
    if crate::bionic::aaudio_selected() {
        let aaudio = table.libraries.entry(AAUDIO_LIBRARY_NAME).or_default();
        for (symbol, address) in crate::bionic::aaudio_overrides() {
            aaudio.push(Entry {
                symbol,
                address,
                source: Source::Cordial,
            });
            table
                .stats
                .entry(AAUDIO_LIBRARY_NAME)
                .or_default()
                .record(Source::Cordial);
        }
    }

    table
}

/// Generic symbols whose host (FreeBSD) libc ABI differs from bionic's, so
/// resolving them from the host would corrupt state rather than help: mutex and
/// rwlock objects have a different size and layout, and syscall(2) numbers are
/// not the same. These stay stubbed until given a real bionic-side translation.
fn abi_unsafe_generic(symbol: &str) -> bool {
    matches!(
        symbol,
        "syscall"
            | "pthread_mutex_init"
            | "pthread_mutex_lock"
            | "pthread_mutex_unlock"
            | "pthread_mutex_trylock"
            | "pthread_mutex_timedlock"
            | "pthread_mutex_destroy"
            | "pthread_rwlock_init"
            | "pthread_rwlock_destroy"
            | "pthread_rwlock_rdlock"
            | "pthread_rwlock_tryrdlock"
            | "pthread_rwlock_wrlock"
            | "pthread_rwlock_trywrlock"
            | "pthread_rwlock_unlock"
    )
}

fn lookup(libs: &[HostLib], symbol: &str) -> Option<(&'static str, *mut c_void)> {
    libs.iter()
        .find_map(|lib| lib.lookup(symbol).map(|addr| (lib.provides, addr)))
}

/// A host shared object consulted for real implementations.
struct HostLib {
    provides: &'static str,
    /// Filename prefix a symbol's defining object must have to count as ours,
    /// e.g. `libm.so` for `libm.so.6`.
    soname_stem: &'static str,
    /// Whether vDSO-provided symbols count as this library's.
    accepts_vdso: bool,
    handle: *mut c_void,
}

impl HostLib {
    fn open(soname: &'static str, provides: &'static str) -> Option<Self> {
        let name = CString::new(soname).ok()?;
        // SAFETY: `name` outlives the call and is NUL-terminated.
        let handle = unsafe { host_dlopen(name.as_ptr(), RTLD_NOW | RTLD_GLOBAL) };
        let soname_stem = soname.split_once(".so").map_or(soname, |(stem, _)| {
            // "libm.so.6" -> "libm.so"
            &soname[..stem.len() + 3]
        });
        (!handle.is_null()).then_some(HostLib {
            provides,
            soname_stem,
            accepts_vdso: soname.starts_with("libc."),
            handle,
        })
    }

    fn lookup(&self, symbol: &str) -> Option<*mut c_void> {
        let name = CString::new(symbol).ok()?;
        // SAFETY: `handle` came from dlopen and is never closed; `name` is valid.
        let addr = unsafe { host_dlsym(self.handle, name.as_ptr()) };
        if addr.is_null() {
            return None;
        }
        // dlsym searches the handle's whole dependency chain, so asking libm for
        // `memcpy` succeeds — glibc's libm.so.6 depends on libc.so.6. Accepting
        // that would attribute 400-odd libc symbols to libm and, worse, silently
        // resolve libc from the host when the caller did not ask for it. Confirm
        // the *defining* object is the one we asked.
        (self.defines(addr)).then_some(addr)
    }

    fn defines(&self, addr: *mut c_void) -> bool {
        let mut info = DlInfo::default();
        // SAFETY: `addr` came from dlsym and `info` is a valid out-parameter.
        if unsafe { host_dladdr(addr, &mut info) } == 0 || info.dli_fname.is_null() {
            return false;
        }
        // SAFETY: dladdr filled dli_fname with a NUL-terminated path.
        let path = unsafe { std::ffi::CStr::from_ptr(info.dli_fname) };
        let path = path.to_string_lossy();
        let file = path.rsplit('/').next().unwrap_or(&path);

        if file.starts_with(self.soname_stem) {
            return true;
        }
        // glibc puts gettimeofday, time and clock_gettime in the vDSO, so dladdr
        // attributes them to linux-vdso.so.1 rather than libc. They are still the
        // implementation libc would have given us.
        self.accepts_vdso && file.starts_with("linux-vdso")
    }
}

/// `Dl_info`, laid out to match glibc's.
#[repr(C)]
struct DlInfo {
    dli_fname: *const c_char,
    dli_fbase: *mut c_void,
    dli_sname: *const c_char,
    dli_saddr: *mut c_void,
}

impl Default for DlInfo {
    fn default() -> Self {
        DlInfo {
            dli_fname: std::ptr::null(),
            dli_fbase: std::ptr::null_mut(),
            dli_sname: std::ptr::null(),
            dli_saddr: std::ptr::null_mut(),
        }
    }
}

// The *host* dynamic loader, not the bionic one. Declared directly rather than
// taking a dependency on the `libc` crate for two functions.
extern "C" {
    #[link_name = "dlopen"]
    fn host_dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    #[link_name = "dlsym"]
    fn host_dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    #[link_name = "dladdr"]
    fn host_dladdr(addr: *mut c_void, info: *mut DlInfo) -> c_int;
}

const RTLD_NOW: c_int = 2;
const RTLD_GLOBAL: c_int = 0x100;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_khronos_by_shape_not_prefix() {
        assert!(matches!(classify("glDrawArrays"), Class::Khronos("libGLESv2.so")));
        assert!(matches!(classify("eglGetDisplay"), Class::Khronos("libEGL.so")));
        // `glob` and `globfree` are libc, not GLES.
        assert!(matches!(classify("glob"), Class::Generic));
        assert!(matches!(classify("globfree"), Class::Generic));
    }

    #[test]
    fn classifies_android_apis() {
        assert!(matches!(classify("ANativeWindow_lock"), Class::Android("libandroid.so")));
        assert!(matches!(classify("__android_log_print"), Class::Android("liblog.so")));
        assert!(matches!(classify("AMediaCodec_start"), Class::Android("libmediandk.so")));
    }

    #[test]
    fn plain_libc_is_generic() {
        assert!(matches!(classify("memcpy"), Class::Generic));
        assert!(matches!(classify("pthread_create"), Class::Generic));
    }

    /// `pthread_once` and thread-specific data must resolve without
    /// `--host-libc`, which is the whole point of implementing them: as stubs
    /// they returned a success the caller could not survive, and the run died
    /// with a SIGSEGV bearing no relation to the call. `build(false)` is the
    /// bare `--lib-dir` configuration.
    #[test]
    fn thread_local_storage_resolves_without_host_libc() {
        let table = build(false);
        let libc = table.libraries.get("libc.so").expect("libc.so registered");
        for symbol in [
            "pthread_once",
            "pthread_key_create",
            "pthread_key_delete",
            "pthread_getspecific",
            "pthread_setspecific",
        ] {
            let entry = libc
                .iter()
                .find(|e| e.symbol == symbol)
                .unwrap_or_else(|| panic!("{symbol} is not in the table at all"));
            assert_eq!(
                entry.source,
                Source::Cordial,
                "{symbol} fell back to a {} — a stub for it returns a lie",
                entry.source.label()
            );
        }
    }
}
