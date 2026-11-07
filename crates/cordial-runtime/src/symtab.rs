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

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{c_char, c_int, c_void, CString};

use crate::elf::Binding;
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

/// libdl's surface, which the bionic linker answers itself.
///
/// **These must never be resolved from the host, and the danger is specific.**
/// glibc exports every one of them, so a host lookup succeeds and hands the
/// guest *our* loader -- at which point the engine's `dlopen("libvulkan.so")`
/// asks glibc for a library only the bionic linker knows about, and gets
/// nothing, with no error that points anywhere near the cause.
///
/// `build.rs` skips the same names when it generates the stub table, so they
/// have never been in `SYMBOLS`. The list is repeated here rather than shared
/// because a build script cannot import from the crate it builds; if one
/// changes, change both.
const LINKER_PROVIDED: &[&str] = &[
    "dlopen",
    "dlsym",
    "dlclose",
    "dlerror",
    "dladdr",
    "dl_iterate_phdr",
    "dlvsym",
];

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

/// An import the engine has and nothing here can answer.
pub struct Unprovidable {
    pub symbol: String,
    /// Weak imports are resolved to zero by the linker and do not stop a load.
    pub binding: Binding,
    /// What sort of thing it looked like, for the reader of the log.
    pub why: String,
}

pub struct SymbolTable {
    pub libraries: BTreeMap<&'static str, Vec<Entry>>,
    pub stats: BTreeMap<&'static str, Stats>,
    /// Host libraries that could not be opened; their symbols fell back to stubs.
    pub missing_host_libs: Vec<&'static str>,
    /// Imports found in the engine that the generated stub table has never
    /// heard of, and which the host answered anyway. Empty on a build the
    /// checked-in symbol list is current for; non-empty is the interesting case
    /// and worth printing.
    pub beyond_stub_table: Vec<(String, &'static str)>,
    /// Imports nothing can answer. A strong one here means the load is about
    /// to fail, and this is the only place that says which symbol and why.
    pub unprovidable: Vec<Unprovidable>,
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

/// The soname a symbol is registered under when nothing more specific applies.
fn fallback_library(class: &Class) -> &'static str {
    match class {
        Class::Android(lib) | Class::Khronos(lib) => lib,
        Class::Generic => "libc.so",
    }
}

/// The Android soname an import belongs under when nothing has claimed it:
/// the prefix table above, and `libc.so` for everything else. Public for the
/// arm64 guest's table (`guest_link`), which buckets names the same way.
pub fn library_for(symbol: &str) -> &'static str {
    fallback_library(&classify(symbol))
}

/// Where a symbol resolves from, before any stub is considered.
///
/// `None` means nothing on this machine can answer it. For a symbol in the
/// generated table that is what the stub is for; for one discovered in the
/// library and absent from the table there is no stub, and `build` records it
/// as unprovidable instead of inventing one.
fn resolve(
    symbol: &str,
    class: &Class,
    overrides: &BTreeMap<&'static str, *mut c_void>,
    host_libs: &[HostLib],
    libc: Option<&HostLib>,
) -> Option<(&'static str, *mut c_void, Source)> {
    // Cordial's own implementations win over everything. They exist because
    // neither the host nor a stub is correct for these; see `bionic`.
    if let Some(&addr) = overrides.get(symbol) {
        return Some((fallback_library(class), addr, Source::Cordial));
    }
    match class {
        // No host has these, so there is nothing to try.
        Class::Android(_) => None,
        // Always register under the Khronos soname even when the host answers,
        // so the bucketing stays honest either way.
        Class::Khronos(lib) => lookup(host_libs, symbol).map(|(_, addr)| (*lib, addr, Source::Host)),
        Class::Generic => match lookup(host_libs, symbol) {
            Some((provides, addr)) => Some((provides, addr, Source::Host)),
            None => libc
                .and_then(|l| l.lookup(symbol))
                .map(|addr| ("libc.so", addr, Source::Host)),
        },
    }
}

/// Build the full table.
///
/// `host_libc` resolves libc symbols from the host as well. It is ABI-unsafe and
/// exists to see how far execution gets, not to be correct.
///
/// `imports` is what the engine's own ELF says it needs, read by `crate::elf`.
/// **It is the answer to issue #15.** The generated stub table is only as
/// current as `docs/analysis/undefined-symbols.tsv`, and twice a Roblox update
/// has added an ordinary libc import the host had all along -- `hypotf`, then
/// `getpwuid_r` -- and stopped the client loading until somebody hand-edited
/// that file. Anything in `imports` and not in `SYMBOLS` is resolved here from
/// the host on the same rules as everything else. What the host cannot answer
/// is *not* given a stub: there is no generated one to give it, and inventing a
/// silent zero is how the next `hypotf` becomes a mystery rather than an error.
/// It goes in `unprovidable`, the load fails on it, and the log names it.
pub fn build(host_libc: bool, imports: &crate::elf::Imports) -> SymbolTable {
    // (host soname, Android soname it stands in for)
    //
    // libstdc++ and libgcc_s are here for one symbol: `__gxx_personality_v0`,
    // the Itanium C++ ABI personality routine. Roblox imports it undefined and
    // throws during static initialisation; with it stubbed the unwinder cannot
    // find a handler, calls std::terminate, and the whole load aborts inside
    // DT_INIT_ARRAY. The ABI is standard, so the host's is the right one.
    let candidates: &[(&'static str, &'static str)] = &[
        ("libm.so.6", "libm.so"),
        ("libz.so.1", "libz.so"),
        ("libGLESv2.so.2", "libGLESv2.so"),
        ("libEGL.so.1", "libEGL.so"),
        ("libstdc++.so.6", "libc.so"),
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
    let libc = host_libc
        .then(|| HostLib::open("libc.so.6", "libc.so"))
        .flatten();

    let mut table = SymbolTable {
        libraries: BTreeMap::new(),
        stats: BTreeMap::new(),
        missing_host_libs,
        beyond_stub_table: Vec::new(),
        unprovidable: Vec::new(),
    };

    for (symbol, stub) in SYMBOLS.iter() {
        let class = classify(symbol);
        let (library, address, source) = resolve(symbol, &class, &overrides, &host_libs, libc.as_ref())
            .unwrap_or((fallback_library(&class), *stub as *mut c_void, Source::Stub));

        table.libraries.entry(library).or_default().push(Entry {
            symbol,
            address,
            source,
        });
        table.stats.entry(library).or_default().record(source);
    }

    // Everything the engine imports that the generated table has never heard
    // of. On a build the checked-in list is current for this loop does nothing;
    // after a Roblox update it is the difference between the client starting
    // and `cannot locate symbol` before any window appears. See `build`'s
    // header and issue #15.
    let known: BTreeSet<&str> = SYMBOLS.iter().map(|(s, _)| *s).collect();
    for (symbol, binding) in imports {
        let name = symbol.as_str();
        if known.contains(name) || LINKER_PROVIDED.contains(&name) {
            continue;
        }
        let class = classify(name);
        match resolve(name, &class, &overrides, &host_libs, libc.as_ref()) {
            Some((library, address, source)) => {
                // The table outlives every caller -- it is handed to the linker
                // at startup and never rebuilt -- and the linker copies these
                // names into `std::string` keys of its own anyway. Leaking is
                // the honest way to say that, and it is bounded by how many
                // imports a Roblox update adds, which has been one at a time.
                let leaked: &'static str = Box::leak(symbol.clone().into_boxed_str());
                table.libraries.entry(library).or_default().push(Entry {
                    symbol: leaked,
                    address,
                    source,
                });
                table.stats.entry(library).or_default().record(source);
                table.beyond_stub_table.push((symbol.clone(), library));
            }
            None => table.unprovidable.push(Unprovidable {
                symbol: symbol.clone(),
                binding: *binding,
                why: match class {
                    Class::Android(lib) => {
                        format!("an Android API belonging to {lib}, which needs implementing here")
                    }
                    Class::Khronos(_) => "no GLES or EGL on this host defines it".to_string(),
                    Class::Generic => "no host library defines it".to_string(),
                },
            }),
        }
    }

    for name in EMPTY_LIBRARIES {
        table.libraries.entry(name).or_default();
        table.stats.entry(name).or_default();
    }

    // Vulkan is `dlopen`ed, not linked — it contributes no undefined symbols to
    // `libroblox.so` and so never reaches the per-symbol classification above.
    //
    // It is deliberately never registered. The virtual `libvulkan.so` and
    // `libvulkan.so.1` sonames do not exist in this table, so the engine's
    // `dlopen` of them fails and it takes its own documented fall-through to
    // GLES3. That is the whole of the graphics backend decision in this fork,
    // and it is baked in: no environment variable, flag or setting offers
    // Vulkan, and the interposition that used to answer it is gone. See
    // `android::vulkan` for the sentinel that fires if a path still reaches.

    // mimalloc: `libroblox.so` contributes no undefined `mi_` symbols (nothing
    // DT_NEEDS it), so it is never reached by the per-symbol classification
    // loop and has to be registered as its own virtual library for whatever
    // `dlopen`s it to find. See `mimalloc_lib` for why this links the real
    // allocator rather than stubbing its option getters, and for what is and
    // is not confirmed about whether the engine ever asks for it.
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
unsafe extern "C" {
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

    /// Issue #15's acceptance test, in miniature: a symbol the host provides
    /// that `docs/analysis/undefined-symbols.tsv` has never mentioned. `hypotl`
    /// is the long-double sibling of the `hypotf` that actually stopped a
    /// client loading in 2.734.0.917, and is not in the file -- checked, not
    /// assumed, by the assertion below.
    #[test]
    fn a_host_symbol_absent_from_the_stub_table_still_resolves() {
        assert!(
            !SYMBOLS.iter().any(|(s, _)| *s == "hypotl"),
            "hypotl has been added to the stub table; this test needs a symbol that has not"
        );

        let mut imports = crate::elf::Imports::new();
        imports.insert("hypotl".to_string(), Binding::Strong);
        let table = build(false, &imports);

        assert!(
            table.unprovidable.is_empty(),
            "libm has hypotl: {:?}",
            table.unprovidable.iter().map(|u| &u.symbol).collect::<Vec<_>>()
        );
        let entry = table
            .libraries
            .get("libm.so")
            .expect("libm.so registered")
            .iter()
            .find(|e| e.symbol == "hypotl")
            .expect("hypotl resolved into libm.so");
        assert_eq!(entry.source, Source::Host);
        assert_eq!(
            table.beyond_stub_table,
            vec![("hypotl".to_string(), "libm.so")]
        );
    }

    /// The other half of the same issue, and the half that must stay loud: a
    /// symbol nothing can answer is reported, not quietly given a zero.
    #[test]
    fn a_symbol_nothing_provides_is_reported_rather_than_stubbed() {
        let name = "cordial_no_such_symbol_anywhere";
        let mut imports = crate::elf::Imports::new();
        imports.insert(name.to_string(), Binding::Strong);
        let table = build(false, &imports);

        assert!(table.beyond_stub_table.is_empty());
        let reported = table
            .unprovidable
            .iter()
            .find(|u| u.symbol == name)
            .expect("reported as unprovidable");
        assert_eq!(reported.binding, Binding::Strong);
        assert!(
            !table
                .libraries
                .values()
                .flatten()
                .any(|e| e.symbol == name),
            "it must not be registered at all -- an unregistered symbol is what makes \
             the linker fail by name, which is the whole point"
        );
    }

    /// glibc exports `dlopen`, so the host lookup for it *succeeds*. Taking it
    /// would hand the guest our loader, and its `dlopen("libvulkan.so")` would
    /// then ask glibc for a library only the bionic linker knows about.
    #[test]
    fn libdl_is_never_taken_from_the_host() {
        let mut imports = crate::elf::Imports::new();
        for name in LINKER_PROVIDED {
            imports.insert(name.to_string(), Binding::Strong);
        }
        let table = build(true, &imports);

        assert!(table.beyond_stub_table.is_empty(), "{:?}", table.beyond_stub_table);
        assert!(table.unprovidable.is_empty(), "nor reported as a gap: the linker has them");
        for name in LINKER_PROVIDED {
            assert!(
                !table.libraries.values().flatten().any(|e| e.symbol == *name),
                "{name} was registered from the host"
            );
        }
    }

    /// A weak import nothing provides is still reported, but marked -- the
    /// eight in `libroblox.so` go unresolved on every healthy launch, and a
    /// message saying the load is about to fail would be wrong every time.
    #[test]
    fn a_weak_gap_is_recorded_as_weak() {
        let mut imports = crate::elf::Imports::new();
        imports.insert("cordial_no_such_weak_hook".to_string(), Binding::Weak);
        let table = build(false, &imports);
        assert_eq!(table.unprovidable.len(), 1);
        assert_eq!(table.unprovidable[0].binding, Binding::Weak);
    }

    /// `pthread_once` and thread-specific data must resolve without
    /// `--host-libc`, which is the whole point of implementing them: as stubs
    /// they returned a success the caller could not survive, and the run died
    /// with a SIGSEGV bearing no relation to the call. `build(false)` is the
    /// bare `--lib-dir` configuration.
    #[test]
    fn thread_local_storage_resolves_without_host_libc() {
        let table = build(false, &Default::default());
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
