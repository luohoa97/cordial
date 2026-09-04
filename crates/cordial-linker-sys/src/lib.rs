//! Bindings to the AOSP bionic linker, retargeted to the host by
//! `mcpelauncher-linker` and wrapped in `native/shim.cpp`.
//!
//! This crate is deliberately thin: it exposes the linker's operations and
//! nothing else. Symbol-table policy — what Cordial provides for each Android
//! library — lives in `cordial-runtime`.

use std::ffi::{c_char, c_int, c_void, CStr, CString};

mod ffi {
    use std::ffi::{c_char, c_int, c_void};

    extern "C" {
        pub fn cordial_linker_init();
        pub fn cordial_linker_load_library(
            name: *const c_char,
            names: *const *const c_char,
            addrs: *const *mut c_void,
            n: usize,
        ) -> *mut c_void;
        pub fn cordial_linker_update_ld_library_path(path: *const c_char);
        pub fn cordial_linker_dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
        // EXPERIMENTAL, cordial-agent-defer: see docs/analysis/flag-init.md
        // §26 and patches/README.md. Not called from the default load path.
        pub fn cordial_linker_defer_next_ctors(defer: c_int);
        pub fn cordial_linker_run_deferred_ctors(handle: *mut c_void);
        // docs/analysis/flag-init.md §31. Metadata only — see the comment on
        // the Rust wrapper below.
        pub fn cordial_linker_set_realpath(handle: *mut c_void, path: *const c_char);
        pub fn cordial_linker_dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        pub fn cordial_linker_dlerror() -> *const c_char;
        pub fn cordial_linker_get_library_base(handle: *mut c_void) -> usize;
        pub fn cordial_linker_get_library_code_region(
            handle: *mut c_void,
            base: *mut usize,
            size: *mut usize,
        );
    }
}

/// `RTLD_NOW` — resolve every relocation at load time. Cordial always uses this:
/// a lazy load would report success and then fail later on an unrelated call.
pub const RTLD_NOW: c_int = 2;
pub const RTLD_LAZY: c_int = 1;

/// A library loaded by, or registered with, the bionic linker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Library(*mut c_void);

// The linker keeps its own global state under its own lock; a handle is just an
// index into it. Sending one between threads is no less safe than using it.
unsafe impl Send for Library {}

impl Library {
    pub fn as_ptr(self) -> *mut c_void {
        self.0
    }

    /// Base address the object was mapped at.
    pub fn base(self) -> usize {
        unsafe { ffi::cordial_linker_get_library_base(self.0) }
    }

    /// Address and length of the executable segment.
    pub fn code_region(self) -> (usize, usize) {
        let (mut base, mut size) = (0usize, 0usize);
        unsafe { ffi::cordial_linker_get_library_code_region(self.0, &mut base, &mut size) };
        (base, size)
    }

    pub fn symbol(self, name: &str) -> Option<*mut c_void> {
        let c = CString::new(name).ok()?;
        let p = unsafe { ffi::cordial_linker_dlsym(self.0, c.as_ptr()) };
        (!p.is_null()).then_some(p)
    }
}

/// Initialise the linker's solist and register its built-in `libdl.so`.
///
/// Must be called once, before anything else in this module.
pub fn init() {
    unsafe { ffi::cordial_linker_init() }
}

/// Register a virtual library: an soname that exists only as the symbol table
/// given here. This is how Cordial provides `libc.so`, `libandroid.so`,
/// `libEGL.so` and the rest — the loaded object's `DT_NEEDED` entries resolve
/// against these instead of against anything on disk.
pub fn register(name: &str, symbols: &[(String, *mut c_void)]) -> Result<Library, Error> {
    // bionic's soinfo::set_soname() stores the pointer it is given rather than
    // copying the string (linker_soinfo.cpp: `soname_ = soname`). AOSP gets away
    // with it because callers pass string literals. A CString dropped at the end
    // of this function would leave every registered library with a dangling
    // soname, and DT_NEEDED lookups would then silently fail to match.
    //
    // So the name is leaked deliberately. There are a dozen of these for the
    // process lifetime.
    let cname: &'static CString = Box::leak(Box::new(CString::new(name)?));

    let cnames = symbols
        .iter()
        .map(|(s, _)| CString::new(s.as_str()))
        .collect::<Result<Vec<_>, _>>()?;
    let name_ptrs: Vec<*const c_char> = cnames.iter().map(|c| c.as_ptr()).collect();
    let addrs: Vec<*mut c_void> = symbols.iter().map(|(_, a)| *a).collect();

    let handle = unsafe {
        ffi::cordial_linker_load_library(
            cname.as_ptr(),
            name_ptrs.as_ptr(),
            addrs.as_ptr(),
            symbols.len(),
        )
    };
    if handle.is_null() {
        Err(Error::Linker(last_error()))
    } else {
        Ok(Library(handle))
    }
}

/// Directory the linker searches for real objects.
pub fn set_library_path(path: &str) -> Result<(), Error> {
    let c = CString::new(path)?;
    unsafe { ffi::cordial_linker_update_ld_library_path(c.as_ptr()) };
    Ok(())
}

/// Load a real ELF object, resolving its imports against previously registered
/// libraries.
pub fn dlopen(soname: &str, flags: c_int) -> Result<Library, Error> {
    let c = CString::new(soname)?;
    let handle = unsafe { ffi::cordial_linker_dlopen(c.as_ptr(), flags) };
    if handle.is_null() {
        Err(Error::Linker(last_error()))
    } else {
        Ok(Library(handle))
    }
}

/// EXPERIMENTAL, cordial-agent-defer: make the *next* [`dlopen`] call map and
/// relocate the object without running its ELF constructors. The caller must
/// follow up with [`run_deferred_ctors`] on the returned [`Library`] once it
/// wants them to run — nothing else runs them.
///
/// This exists to test whether `libroblox.so`'s constructors (which is where
/// `RbxStorage::init` lives — see `docs/analysis/flag-init.md` §26) can be
/// deferred past Cordial's own directory setup, which currently happens only
/// after `dlopen` returns. It is not wired into the default load path in
/// `cordial-run`; nothing calls this outside an explicit experiment.
pub fn defer_next_ctors(defer: bool) {
    unsafe { ffi::cordial_linker_defer_next_ctors(defer as c_int) }
}

/// EXPERIMENTAL, cordial-agent-defer: run whatever construction
/// [`defer_next_ctors`] left pending for `lib`. Idempotent — the underlying
/// `soinfo::call_constructors()` is itself guarded, so calling this on a
/// library that was never deferred (or already constructed) is harmless.
pub fn run_deferred_ctors(lib: Library) {
    unsafe { ffi::cordial_linker_run_deferred_ctors(lib.0) }
}

/// docs/analysis/flag-init.md §31: overrides what `dladdr()` reports as
/// `lib`'s own path (`Dl_info::dli_fname`), by writing the linker's internal
/// `soinfo::realpath_` directly. Nothing is reopened, remapped, or copied —
/// every byte the engine reads still comes from wherever it was actually
/// mapped from. Meant to be called after [`defer_next_ctors`] +
/// [`dlopen`] and before [`run_deferred_ctors`], so the override is visible
/// to any constructor-time code that asks the linker "what is my own path" —
/// which is the one form of self-location available before `JNI_OnLoad`,
/// since `RbxStorage::init`'s failing `stat("")` calls run during ELF
/// construction, strictly earlier.
pub fn set_realpath(lib: Library, path: &str) {
    let Ok(c) = CString::new(path) else { return };
    // SAFETY: `cordial_linker_set_realpath` copies the string; `c` need not
    // outlive the call.
    unsafe { ffi::cordial_linker_set_realpath(lib.0, c.as_ptr()) }
}

fn last_error() -> String {
    let p = unsafe { ffi::cordial_linker_dlerror() };
    if p.is_null() {
        "unknown linker error".into()
    } else {
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

#[derive(Debug)]
pub enum Error {
    Linker(String),
    NulByte(std::ffi::NulError),
}

impl From<std::ffi::NulError> for Error {
    fn from(e: std::ffi::NulError) -> Self {
        Error::NulByte(e)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Linker(s) => write!(f, "{s}"),
            Error::NulByte(e) => write!(f, "invalid name: {e}"),
        }
    }
}

impl std::error::Error for Error {}

/// The JNI virtual machine Roblox's native code calls back into.
///
/// Roblox registers 518 natives statically, but the traffic that matters runs the
/// other way: native code reaching for Java classes it expects Android to
/// provide. libjnivm answers those calls and records what was asked for, which is
/// how the framework-API backlog stops being a guess.
pub mod jni {
    use std::ffi::{c_char, c_int, c_void, CString};

    extern "C" {
        fn cordial_jni_create_vm() -> *mut c_void;
        fn cordial_jni_env() -> *mut c_void;
        fn cordial_jni_dump_classes(path: *const c_char) -> c_int;
        fn cordial_jni_call_onload(f: *mut c_void, err: *mut c_char, err_len: usize) -> c_int;
    }

    /// Call Roblox's `JNI_OnLoad` with the process JavaVM.
    ///
    /// Any C++ exception is caught on the far side: letting one cross the FFI
    /// boundary gives a core dump and no explanation.
    pub fn call_on_load(f: *mut c_void) -> Result<i32, String> {
        let mut err = vec![0u8; 512];
        // SAFETY: `f` is libroblox's JNI_OnLoad export; `err` is a live buffer of
        // the length passed alongside it.
        let rc = unsafe { cordial_jni_call_onload(f, err.as_mut_ptr() as *mut c_char, err.len()) };
        match rc {
            -1 => Err("no JavaVM, or JNI_OnLoad not found".into()),
            -2 | -3 => {
                let end = err.iter().position(|&b| b == 0).unwrap_or(err.len());
                Err(String::from_utf8_lossy(&err[..end]).into_owned())
            }
            v => Ok(v),
        }
    }

    /// Create the process's `JavaVM`. Returns `None` if one already exists.
    pub fn create_vm() -> Option<*mut c_void> {
        // SAFETY: the VM is process-global and owned by the shim.
        let vm = unsafe { cordial_jni_create_vm() };
        (!vm.is_null()).then_some(vm)
    }

    /// The calling thread's `JNIEnv*`.
    pub fn env() -> Option<*mut c_void> {
        // SAFETY: returns null when no VM exists, which is checked.
        let env = unsafe { cordial_jni_env() };
        (!env.is_null()).then_some(env)
    }

    /// Write C++ stubs for every Java class and method the native code reached
    /// for. This is the observed Phase 2 backlog.
    pub fn dump_classes(path: &str) -> Result<(), String> {
        let c = CString::new(path).map_err(|e| e.to_string())?;
        // SAFETY: `c` is a valid NUL-terminated path for the duration of the call.
        match unsafe { cordial_jni_dump_classes(c.as_ptr()) } {
            0 => Ok(()),
            -1 => Err("no JavaVM has been created".into()),
            -2 => Err("libjnivm was built without JNI_DEBUG".into()),
            n => Err(format!("class dump failed ({n})")),
        }
    }
}

/// Drive AGDK `GameActivity` bring-up.
///
/// On Android the platform calls `initializeNativeCode` from Java with a real
/// Activity. Cordial builds the arguments through libjnivm and calls the
/// exported JNI native directly. The returned handle is what every later
/// callback carries — surface creation, resize, input.
pub mod game_activity {
    use std::ffi::{c_char, c_int, c_void, CString};

    extern "C" {
        fn cordial_game_activity_init(
            f: *mut c_void,
            internal_path: *const c_char,
            obb_path: *const c_char,
            external_path: *const c_char,
            err: *mut c_char,
            err_len: usize,
        ) -> i64;
    }

    extern "C" {
        fn cordial_set_bootstrap(f: Option<extern "C" fn()>);
    }

    /// Install what `GameActivity.bootstrapTheApp()` runs.
    ///
    /// The engine calls that method from inside `initializeNativeCode` and reads
    /// its flags verdict on the next line, so this has to be installed before
    /// [`init`] rather than after it. Delivering the settings after
    /// `initializeNativeCode` returned is what Cordial did for months, and it is
    /// why the verdict was always `onFlagsFailed` no matter what the document
    /// contained: the engine had already asked and been told nothing.
    ///
    /// Passing `None` restores the previous behaviour, which is the control for
    /// any measurement of this.
    pub fn set_bootstrap(f: Option<extern "C" fn()>) {
        // SAFETY: stores a function pointer the C++ side only ever reads.
        unsafe { cordial_set_bootstrap(f) }
    }

    extern "C" {
        fn cordial_game_activity_start(
            handle: i64,
            width: c_int,
            height: c_int,
            format: c_int,
            err: *mut c_char,
            err_len: usize,
        ) -> c_int;
    }

    /// Drive the Activity lifecycle and hand the engine its surface.
    pub fn start(handle: i64, width: i32, height: i32, format: i32) -> Result<(), String> {
        let mut err = vec![0u8; 512];
        // SAFETY: `handle` came from `initialize`; `err` is a live buffer.
        let rc = unsafe {
            cordial_game_activity_start(
                handle,
                width,
                height,
                format,
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            let end = err.iter().position(|&b| b == 0).unwrap_or(err.len());
            Err(String::from_utf8_lossy(&err[..end]).into_owned())
        }
    }

    extern "C" {
        fn cordial_set_init_params(
            f: *mut c_void,
            assets: *const c_char,
            width: c_int,
            height: c_int,
            err: *mut c_char,
            err_len: usize,
        ) -> c_int;
    }

    /// `MainGameActivity.nativeAppBridgeSetInitParams` — where the service lives,
    /// what the device is, and what the viewport looks like. The engine renders
    /// its own app shell and draws nothing until it has these.
    pub fn set_init_params(
        native: *mut c_void,
        assets: &str,
        width: i32,
        height: i32,
    ) -> Result<(), String> {
        let a = CString::new(assets).map_err(|e| e.to_string())?;
        let mut err = vec![0u8; 512];
        // SAFETY: `native` is the exported JNI native; `a` outlives the call.
        let rc = unsafe {
            cordial_set_init_params(
                native,
                a.as_ptr(),
                width,
                height,
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            let end = err.iter().position(|&b| b == 0).unwrap_or(err.len());
            Err(String::from_utf8_lossy(&err[..end]).into_owned())
        }
    }

    extern "C" {
        fn cordial_asset_manager_init(f: *mut c_void, err: *mut c_char, n: usize) -> c_int;
        fn cordial_storage_init(
            f: *mut c_void,
            a: *const c_char,
            b: *const c_char,
            err: *mut c_char,
            n: usize,
        ) -> c_int;
        fn cordial_call_bare(f: *mut c_void, err: *mut c_char, n: usize) -> c_int;
        fn cordial_init_flags(
            f: *mut c_void,
            settings: *const c_char,
            err: *mut c_char,
            n: usize,
        ) -> c_int;
        fn cordial_appbridge_init(
            f: *mut c_void,
            assets: *const c_char,
            w: c_int,
            h: c_int,
            err: *mut c_char,
            n: usize,
        ) -> c_int;
        fn cordial_appbridge_call_bare(f: *mut c_void, err: *mut c_char, n: usize) -> c_int;
        fn cordial_read_local_flags(f: *mut c_void, err: *mut c_char, n: usize) -> c_int;
        fn cordial_appbridge_call_bare_cls(
            f: *mut c_void,
            class_name: *const c_char,
            err: *mut c_char,
            n: usize,
        ) -> c_int;
        fn cordial_init_client_settings(
            f: *mut c_void,
            a: *const c_char,
            b: *const c_char,
            c: *const c_char,
            out_result: *mut c_int,
            err: *mut c_char,
            n: usize,
        ) -> c_int;
        fn cordial_init_client_settings_cached_compressed(
            f: *mut c_void,
            data: *const u8,
            len: usize,
            a: *const c_char,
            b: *const c_char,
            c: *const c_char,
            when: i64,
            flag: c_int,
            out_result: *mut c_int,
            err: *mut c_char,
            n: usize,
        ) -> c_int;
        fn cordial_set_display_size(width: c_int, height: c_int);
        fn cordial_set_ui_mode_night(night: c_int);
        fn cordial_get_fint(
            f: *mut c_void,
            name: *const c_char,
            fallback: c_int,
            out_result: *mut c_int,
            err: *mut c_char,
            n: usize,
        ) -> c_int;
        fn cordial_post_client_settings_loaded(f: *mut c_void, err: *mut c_char, n: usize)
            -> c_int;
        fn cordial_preload_flag_overrides(
            f: *mut c_void,
            json: *const c_char,
            err: *mut c_char,
            n: usize,
        ) -> c_int;
        fn cordial_call_static_strings(
            f: *mut c_void,
            class_name: *const c_char,
            args: *const *const c_char,
            n: usize,
            err: *mut c_char,
            n_err: usize,
        ) -> c_int;
        fn cordial_call_static_bool_string(
            f: *mut c_void,
            class_name: *const c_char,
            flag: c_int,
            text: *const c_char,
            err: *mut c_char,
            n: usize,
        ) -> c_int;
        fn cordial_set_device_info(
            f: *mut c_void,
            width: c_int,
            height: c_int,
            err: *mut c_char,
            n: usize,
        ) -> c_int;
        fn cordial_activity_lifecycle(
            f: *mut c_void,
            activity: *const c_char,
            err: *mut c_char,
            n: usize,
        ) -> c_int;
        fn cordial_appbridge_start_app(
            f: *mut c_void,
            assets: *const c_char,
            w: c_int,
            h: c_int,
            err: *mut c_char,
            n: usize,
        ) -> c_int;
        fn cordial_appbridge_update_surface_app(
            f: *mut c_void,
            assets: *const c_char,
            w: c_int,
            h: c_int,
            err: *mut c_char,
            n: usize,
        ) -> c_int;
        fn cordial_appbridge_update_surface_game(
            f: *mut c_void,
            assets: *const c_char,
            w: c_int,
            h: c_int,
            err: *mut c_char,
            n: usize,
        ) -> c_int;
        fn cordial_call_static_bare_bool(
            f: *mut c_void,
            class_name: *const c_char,
            out_result: *mut c_int,
            err: *mut c_char,
            n: usize,
        ) -> c_int;
        fn cordial_cookies_set_host_sink(sink: Option<extern "C" fn(*const c_char)>);
        fn cordial_cookies_register_handler(f: *mut c_void, err: *mut c_char, n: usize) -> c_int;
        fn cordial_identity_set_sinks(
            on_login: Option<extern "C" fn(*const c_char)>,
            on_logout: Option<extern "C" fn()>,
        );
        fn cordial_identity_publish(
            user_id: i64,
            username: *const c_char,
            display_name: *const c_char,
            membership_type: i64,
            is_under13: c_int,
            has_subscription: c_int,
        );
        fn cordial_identity_clear();
        fn cordial_cookies_get_for_domain(
            f: *mut c_void,
            class_name: *const c_char,
            domain: *const c_char,
            out: *mut c_char,
            out_len: usize,
            needed: *mut usize,
            err: *mut c_char,
            n: usize,
        ) -> c_int;
        fn cordial_pass_current_refresh_rate(
            f: *mut c_void,
            hz: f32,
            err: *mut c_char,
            n: usize,
        ) -> c_int;
        fn cordial_pass_supported_refresh_rates(
            f: *mut c_void,
            rates: *const f32,
            count: usize,
            err: *mut c_char,
            n: usize,
        ) -> c_int;
        fn cordial_deeplink_protocol_string(
            f: *mut c_void,
            class_name: *const c_char,
            out: *mut c_char,
            out_len: usize,
            err: *mut c_char,
            n: usize,
        ) -> c_int;
        fn cordial_deeplink_cold_start(
            f: *mut c_void,
            class_name: *const c_char,
            url: *const c_char,
            out_handled: *mut c_int,
            err: *mut c_char,
            n: usize,
        ) -> c_int;
        fn cordial_deeplink_protocol_init(
            f: *mut c_void,
            class_name: *const c_char,
            err: *mut c_char,
            n: usize,
        ) -> c_int;
        fn cordial_deeplink_two_strings_ret_string(
            f: *mut c_void,
            class_name: *const c_char,
            arg_a: *const c_char,
            arg_b: *const c_char,
            out: *mut c_char,
            out_len: usize,
            err: *mut c_char,
            n: usize,
        ) -> c_int;
        fn cordial_deeplink_string_ret_string(
            f: *mut c_void,
            class_name: *const c_char,
            arg: *const c_char,
            out: *mut c_char,
            out_len: usize,
            err: *mut c_char,
            n: usize,
        ) -> c_int;
        fn cordial_app_ready_set_sink(on_ready: Option<extern "C" fn(*const c_char)>);
        fn cordial_report_battery_state_changed(
            f: *mut c_void,
            status: c_int,
            plugged: c_int,
            err: *mut c_char,
            n: usize,
        ) -> c_int;
        fn cordial_report_battery_status(
            f: *mut c_void,
            status: *const CordialBatteryStatus,
            err: *mut c_char,
            n: usize,
        ) -> c_int;
    }

    fn take_err(err: Vec<u8>) -> String {
        let end = err.iter().position(|&b| b == 0).unwrap_or(err.len());
        String::from_utf8_lossy(&err[..end]).into_owned()
    }

    /// `JNIAAssetManagerSetup.initNative` — hands the engine its asset manager.
    pub fn asset_manager_init(native: *mut c_void) -> Result<(), String> {
        let mut err = vec![0u8; 512];
        // SAFETY: `native` is the exported JNI native; `err` is a live buffer.
        let rc = unsafe {
            cordial_asset_manager_init(native, err.as_mut_ptr() as *mut c_char, err.len())
        };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    /// `LocalStorageManager.initStorageManagerNativeV3`.
    pub fn storage_init(native: *mut c_void, a: &str, b: &str) -> Result<(), String> {
        let ca = CString::new(a).map_err(|e| e.to_string())?;
        let cb = CString::new(b).map_err(|e| e.to_string())?;
        let mut err = vec![0u8; 512];
        // SAFETY: as above; both paths outlive the call.
        let rc = unsafe {
            cordial_storage_init(
                native,
                ca.as_ptr(),
                cb.as_ptr(),
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    /// A static native on a named class taking up to three `String` arguments.
    ///
    /// `NativeSettingsInterface.nativeSetFilesDirectory` and friends are how the
    /// app tells the engine which directories it owns. Nothing here called them,
    /// so the engine resolved `appData`, `cache`, `http` and `sounds` against the
    /// working directory instead of absolute storage.
    pub fn call_static_strings(
        native: *mut c_void,
        class_name: &str,
        args: &[&str],
    ) -> Result<(), String> {
        let cls = CString::new(class_name).map_err(|e| e.to_string())?;
        let owned: Vec<CString> = args
            .iter()
            .map(|a| CString::new(*a).map_err(|e| e.to_string()))
            .collect::<Result<_, _>>()?;
        let ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
        let mut err = vec![0u8; 512];
        // SAFETY: `native` is the exported JNI native; every buffer outlives the call.
        let rc = unsafe {
            cordial_call_static_strings(
                native,
                cls.as_ptr(),
                ptrs.as_ptr(),
                ptrs.len(),
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    /// `LocalStorageManager.initStorageManagerNativeV3(AssetManager, String, String)`
    ///
    /// The engine's content store. See the C++ side for why this exists and what
    /// about the two paths is still unestablished.
    pub fn init_storage_manager(native: *mut c_void, a: &str, b: &str) -> Result<(), String> {
        let ca = CString::new(a).map_err(|e| e.to_string())?;
        let cb = CString::new(b).map_err(|e| e.to_string())?;
        let mut err = vec![0u8; 512];
        // SAFETY: `native` is the exported JNI native; both buffers outlive the call.
        let rc = unsafe {
            cordial_init_storage_manager(
                native,
                ca.as_ptr(),
                cb.as_ptr(),
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    /// A static, zero-argument native returning `boolean`. Added purely to
    /// observe `NativeSettingsInterface.nativeIsLuaLoginEnabled()`'s own
    /// verdict for `docs/design/sign-in.md` — diagnostic-only, does not drive
    /// any UI or enter any credentials.
    pub fn call_static_bare_bool(native: *mut c_void, class_name: &str) -> Result<bool, String> {
        let cls = CString::new(class_name).map_err(|e| e.to_string())?;
        let mut out: c_int = -1;
        let mut err = vec![0u8; 512];
        // SAFETY: `native` is the exported JNI native; every buffer outlives the call.
        let rc = unsafe {
            cordial_call_static_bare_bool(
                native,
                cls.as_ptr(),
                &mut out as *mut c_int,
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };
        if rc == 0 { Ok(out != 0) } else { Err(take_err(err)) }
    }

    /// `NativeGLInterface.nativePassCurrentDisplayRefreshRate(F)V`.
    ///
    /// Which rate to send when a window is on two outputs at once is decided in
    /// `cordial_runtime::refresh`, not here.
    pub fn pass_current_refresh_rate(native: *mut c_void, hz: f32) -> Result<(), String> {
        let mut err = vec![0u8; 512];
        // SAFETY: `native` is the exported JNI native; the buffer outlives the call.
        let rc = unsafe {
            cordial_pass_current_refresh_rate(native, hz, err.as_mut_ptr() as *mut c_char, err.len())
        };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    /// `NativeGLInterface.nativePassSupportedRefreshRates([F)V`.
    ///
    /// An empty slice is refused rather than sent. "Every rate this display
    /// supports, and there are none" is not a thing to tell a renderer, and the
    /// engine has been managing without the call at all — so saying nothing
    /// remains strictly better than saying that.
    pub fn pass_supported_refresh_rates(native: *mut c_void, rates: &[f32]) -> Result<(), String> {
        if rates.is_empty() {
            return Err("no plausible refresh rates to report".into());
        }
        let mut err = vec![0u8; 512];
        // SAFETY: `native` is the exported JNI native; `rates` and the error
        // buffer both outlive the call.
        let rc = unsafe {
            cordial_pass_supported_refresh_rates(
                native,
                rates.as_ptr(),
                rates.len(),
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    /// `NativeGLInterface.reportBatteryStateChanged(II)V`. `status` and
    /// `plugged` are Android's own `BatteryManager` raw values — see
    /// `crates/cordial-runtime/src/battery.rs` for where they came from.
    pub fn report_battery_state_changed(
        native: *mut c_void,
        status: i32,
        plugged: i32,
    ) -> Result<(), String> {
        let mut err = vec![0u8; 512];
        // SAFETY: `native` is the exported JNI native; the buffer outlives the call.
        let rc = unsafe {
            cordial_report_battery_state_changed(
                native,
                status,
                plugged,
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    /// The Rust-friendly, `Option`-per-field shape of a `BatteryStatus`
    /// reading — this crate's own type, not borrowed from `cordial-runtime`
    /// (which depends on this crate, not the other way round; a shared type
    /// would need the dependency to point the wrong way). A caller ordinarily
    /// builds this by copying `cordial_runtime::battery::Reading`'s fields
    /// across one for one — a field-for-field copy, not a translation, for the
    /// same "no logic on the wrong side of a crate wall" reason
    /// `cordial_shell::refresh_watch`'s own `Output` type gives.
    ///
    /// `None` means the same thing here as it does in `battery.rs`: this
    /// machine's sysfs did not answer the question, so the field is left null
    /// on the Java side rather than sent as a guessed zero.
    #[derive(Debug, Clone, Default, PartialEq)]
    pub struct BatteryStatusFields {
        pub present: Option<bool>,
        pub percentage: Option<i32>,
        pub status: Option<i32>,
        pub health: Option<i32>,
        pub voltage_mv: Option<i32>,
        pub current_now_ua: Option<i32>,
        pub current_avg_ua: Option<i32>,
        pub charge_counter_uah: Option<i32>,
        pub power_now_uw: Option<i32>,
        pub technology: Option<String>,
        pub temperature_c: Option<f32>,
        pub plugged: Option<i32>,
    }

    /// The `extern "C"` shape, mirroring `struct CordialBatteryStatus` in
    /// `native/battery.cpp` field for field — see that file for why each
    /// field carries its own `has_*` flag rather than a sentinel value.
    /// Private to this module: [`BatteryStatusFields`] is the public surface,
    /// and this is only the wire format `report_battery_status` builds on the
    /// way to the call.
    #[repr(C)]
    struct CordialBatteryStatus {
        has_present: i32,
        present: i32,
        has_percentage: i32,
        percentage: i32,
        has_status: i32,
        status: i32,
        has_health: i32,
        health: i32,
        has_voltage_mv: i32,
        voltage_mv: i32,
        has_current_now_ua: i32,
        current_now_ua: i32,
        has_current_avg_ua: i32,
        current_avg_ua: i32,
        has_charge_counter_uah: i32,
        charge_counter_uah: i32,
        has_power_now_uw: i32,
        power_now_uw: i32,
        has_technology: i32,
        technology: *const c_char,
        has_temperature_c: i32,
        temperature_c: f32,
        has_plugged: i32,
        plugged: i32,
    }

    impl Default for CordialBatteryStatus {
        fn default() -> Self {
            // Every `has_*` flag starts clear and every value starts zeroed —
            // the all-null `BatteryStatus` the engine gets if a caller sets
            // nothing, which is the honest reading for "nothing was measured"
            // rather than any particular zero being mistaken for a real one.
            unsafe { std::mem::zeroed() }
        }
    }

    /// `NativeGLInterface.reportBatteryStatus(Lcom/roblox/engine/jni/model/BatteryStatus;)V`.
    pub fn report_battery_status(
        native: *mut c_void,
        status: &BatteryStatusFields,
    ) -> Result<(), String> {
        let technology_c;
        let mut raw = CordialBatteryStatus::default();
        if let Some(v) = status.present {
            raw.has_present = 1;
            raw.present = v as i32;
        }
        if let Some(v) = status.percentage {
            raw.has_percentage = 1;
            raw.percentage = v;
        }
        if let Some(v) = status.status {
            raw.has_status = 1;
            raw.status = v;
        }
        if let Some(v) = status.health {
            raw.has_health = 1;
            raw.health = v;
        }
        if let Some(v) = status.voltage_mv {
            raw.has_voltage_mv = 1;
            raw.voltage_mv = v;
        }
        if let Some(v) = status.current_now_ua {
            raw.has_current_now_ua = 1;
            raw.current_now_ua = v;
        }
        if let Some(v) = status.current_avg_ua {
            raw.has_current_avg_ua = 1;
            raw.current_avg_ua = v;
        }
        if let Some(v) = status.charge_counter_uah {
            raw.has_charge_counter_uah = 1;
            raw.charge_counter_uah = v;
        }
        if let Some(v) = status.power_now_uw {
            raw.has_power_now_uw = 1;
            raw.power_now_uw = v;
        }
        if let Some(t) = &status.technology {
            technology_c = CString::new(t.as_str()).map_err(|e| e.to_string())?;
            raw.has_technology = 1;
            raw.technology = technology_c.as_ptr();
        }
        if let Some(v) = status.temperature_c {
            raw.has_temperature_c = 1;
            raw.temperature_c = v;
        }
        if let Some(v) = status.plugged {
            raw.has_plugged = 1;
            raw.plugged = v;
        }

        let mut err = vec![0u8; 512];
        // SAFETY: `native` is the exported JNI native; `raw` and the `CString`
        // backing `raw.technology` both outlive this call.
        let rc = unsafe {
            cordial_report_battery_status(
                native,
                &raw as *const CordialBatteryStatus,
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    /// A static, zero-argument native returning `String`.
    ///
    /// `JNILinkingProtocol`'s message and field names are read this way — see
    /// `native/deeplink.cpp`. Purely a read of a constant the engine already
    /// holds; nothing is passed in.
    pub fn call_static_ret_string(native: *mut c_void, class_name: &str) -> Result<String, String> {
        let cls = CString::new(class_name).map_err(|e| e.to_string())?;
        let mut out = vec![0u8; 512];
        let mut err = vec![0u8; 512];
        // SAFETY: `native` is the exported JNI native; every buffer outlives the call.
        let rc = unsafe {
            cordial_deeplink_protocol_string(
                native,
                cls.as_ptr(),
                out.as_mut_ptr() as *mut c_char,
                out.len(),
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };
        if rc == 0 { Ok(take_err(out)) } else { Err(take_err(err)) }
    }

    /// `maybeHandleColdStartProtocolLaunch(String) -> boolean`, on whichever of
    /// `JNIBaseUrlProtocol` / `JNIWebLoginProtocol` is named.
    ///
    /// The returned boolean is the engine's own answer to "did I take this
    /// URL", and it is the only honest signal Cordial has about a deep link.
    pub fn cold_start_protocol_launch(
        native: *mut c_void,
        class_name: &str,
        url: &str,
    ) -> Result<bool, String> {
        let cls = CString::new(class_name).map_err(|e| e.to_string())?;
        let u = CString::new(url).map_err(|e| e.to_string())?;
        let mut out: c_int = -1;
        let mut err = vec![0u8; 512];
        // SAFETY: `native` is the exported JNI native; every buffer outlives the call.
        let rc = unsafe {
            cordial_deeplink_cold_start(
                native,
                cls.as_ptr(),
                u.as_ptr(),
                &mut out as *mut c_int,
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };
        if rc == 0 { Ok(out != 0) } else { Err(take_err(err)) }
    }

    /// `init(Context)` on one of the linking protocol classes.
    pub fn protocol_init(native: *mut c_void, class_name: &str) -> Result<(), String> {
        let cls = CString::new(class_name).map_err(|e| e.to_string())?;
        let mut err = vec![0u8; 512];
        // SAFETY: `native` is the exported JNI native; `cls`/`err` outlive the call.
        let rc = unsafe {
            cordial_deeplink_protocol_init(
                native,
                cls.as_ptr(),
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    /// A static native taking one `String` and returning one —
    /// `MessageBus.getLastRaw(String)`, which is how a publish is checked
    /// rather than assumed.
    /// A static native taking two `String`s and returning `String`.
    ///
    /// `MessageBus.getMessageId(protocolName, methodId)` composes a bus id this
    /// way. Asking the engine to compose it is the point: a subscriber that
    /// spelled the id itself would be guessing at a constant the engine owns,
    /// and would find out by never receiving anything.
    pub fn call_static_two_strings_ret_string(
        native: *mut c_void,
        class_name: &str,
        a: &str,
        b: &str,
    ) -> Result<String, String> {
        let cls = CString::new(class_name).map_err(|e| e.to_string())?;
        let ca = CString::new(a).map_err(|e| e.to_string())?;
        let cb = CString::new(b).map_err(|e| e.to_string())?;
        let mut out = vec![0u8; 512];
        let mut err = vec![0u8; 512];
        // SAFETY: `native` is the exported JNI native; every buffer outlives the call.
        let rc = unsafe {
            cordial_deeplink_two_strings_ret_string(
                native,
                cls.as_ptr(),
                ca.as_ptr(),
                cb.as_ptr(),
                out.as_mut_ptr() as *mut c_char,
                out.len(),
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };
        if rc == 0 {
            let n = out.iter().position(|b| *b == 0).unwrap_or(out.len());
            Ok(String::from_utf8_lossy(&out[..n]).into_owned())
        } else {
            Err(take_err(err))
        }
    }

    pub fn call_static_string_ret_string(
        native: *mut c_void,
        class_name: &str,
        arg: &str,
    ) -> Result<String, String> {
        let cls = CString::new(class_name).map_err(|e| e.to_string())?;
        let a = CString::new(arg).map_err(|e| e.to_string())?;
        // Generous, because a bus payload is JSON and truncation is reported as
        // an error rather than silently returning a prefix that still parses.
        let mut out = vec![0u8; 8192];
        let mut err = vec![0u8; 512];
        // SAFETY: `native` is the exported JNI native; every buffer outlives the call.
        let rc = unsafe {
            cordial_deeplink_string_ret_string(
                native,
                cls.as_ptr(),
                a.as_ptr(),
                out.as_mut_ptr() as *mut c_char,
                out.len(),
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };
        if rc == 0 { Ok(take_err(out)) } else { Err(take_err(err)) }
    }

    /// Install the sink `APP_READY` is reported to, or clear it with `None`.
    ///
    /// The payload is the app-shell state the engine reached — `Landing`,
    /// `Home` and so on. It is not personal data; the notification that is
    /// (`DID_LOG_IN`) goes to a different sink and is elided there.
    pub fn app_ready_set_sink(on_ready: Option<extern "C" fn(*const c_char)>) {
        // SAFETY: the far side stores the pointer in an atomic and calls it
        // from the engine's thread; a null clears it.
        unsafe { cordial_app_ready_set_sink(on_ready) }
    }

    /// Hand the framework layer the identity its Java mirrors answer from.
    ///
    /// `NativeUserJavaInterface` and `StartAppParams` both report who is signed
    /// in, and both used to report nobody. They live in `native/`, not in
    /// `libroblox.so`, so this call takes no engine symbol and has no ordering
    /// constraint against the engine — only against
    /// `nativeAppBridgeV2StartAppWithParams`, which copies four of these fields
    /// into the app-start parameters once and never asks again.
    ///
    /// A username identifies a person. It crosses this boundary as bytes and is
    /// never printed on either side; see `crate::identity` in `cordial-runtime`
    /// for the rest of that reasoning.
    pub fn identity_publish(
        user_id: i64,
        username: &str,
        display_name: &str,
        membership_type: i64,
        is_under13: bool,
        has_subscription: bool,
    ) {
        // A name carrying an interior nul is not something the engine produces,
        // and truncating one would publish a different account than the one that
        // signed in. Refused whole instead.
        let (Ok(user), Ok(display)) = (CString::new(username), CString::new(display_name)) else {
            return;
        };
        // SAFETY: both pointers are valid for the duration of the call, and the
        // C side copies out of them before returning.
        unsafe {
            cordial_identity_publish(
                user_id,
                user.as_ptr(),
                display.as_ptr(),
                membership_type,
                is_under13 as c_int,
                has_subscription as c_int,
            )
        }
    }

    /// Put the mirrors back to reporting nobody, on a logout.
    pub fn identity_clear() {
        // SAFETY: no arguments, and the C side takes its own lock.
        unsafe { cordial_identity_clear() }
    }

    /// Install the sinks the DataModel notification handler reports through.
    ///
    /// `on_login` receives a `DID_LOG_IN` payload; `on_logout` is called with
    /// nothing on a `DID_LOG_OUT`. Both are plain `extern "C"` functions with
    /// static lifetime, which is why the C side can hold them for the life of
    /// the process.
    pub fn identity_set_sinks(
        on_login: extern "C" fn(*const c_char),
        on_logout: extern "C" fn(),
    ) {
        // SAFETY: both are static function pointers with C ABI.
        unsafe { cordial_identity_set_sinks(Some(on_login), Some(on_logout)) }
    }

    /// A cookie jar on its way between the engine and the profile directory.
    ///
    /// A newtype rather than a `String` because the difference matters exactly
    /// once, in the diagnostic that gets added at three in the morning. The
    /// value is a live session: printing it to a log, a trace or a panic
    /// message hands somebody's account to whoever reads that log. `Debug`
    /// therefore reports the length and nothing else, so the careless thing to
    /// write is also the safe thing, and getting at the real bytes takes a
    /// deliberate call to [`Jar::expose`].
    pub struct Jar(String);

    impl Jar {
        /// The bytes, for handing back to the engine or writing to the profile.
        /// Every caller of this is a place to check for a leak.
        pub fn expose(&self) -> &str {
            &self.0
        }

        pub fn len(&self) -> usize {
            self.0.len()
        }

        pub fn is_empty(&self) -> bool {
            self.0.is_empty()
        }

        pub fn from_stored(value: String) -> Self {
            Jar(value)
        }
    }

    impl std::fmt::Debug for Jar {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "Jar({} bytes)", self.0.len())
        }
    }

    /// `JNICookieProtocol.updateOnSetCookieHandler` — hand the engine an object
    /// to call when a response carries `Set-Cookie`.
    ///
    /// `sink` receives the *host* the cookies came from, never the cookies. See
    /// `native/cookies.cpp` for why that split is where it is: the host is
    /// extracted before anything else reads the URL, because the query string
    /// of a Roblox URL can carry a one-time authentication ticket.
    pub fn cookies_register_handler(
        native: *mut c_void,
        sink: extern "C" fn(*const c_char),
    ) -> Result<(), String> {
        let mut err = vec![0u8; 512];
        // SAFETY: `native` is the exported JNI native; `sink` is a plain
        // `extern "C"` fn with static lifetime, and `err` outlives the call.
        let rc = unsafe {
            cordial_cookies_set_host_sink(Some(sink));
            cordial_cookies_register_handler(native, err.as_mut_ptr() as *mut c_char, err.len())
        };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    /// `NativeSettingsInterface.nativeGetCookiesForDomain(String) -> String`.
    ///
    /// The buffer is large because the alternative is worse. A jar that does
    /// not fit is reported as an error naming its size rather than truncated:
    /// half a cookie still parses as a cookie, and the engine would accept it
    /// on the next launch and fail authentication for a reason with no visible
    /// relationship to a buffer.
    pub fn cookies_for_domain(
        native: *mut c_void,
        class_name: &str,
        domain: &str,
    ) -> Result<Jar, String> {
        let cls = CString::new(class_name).map_err(|e| e.to_string())?;
        let dom = CString::new(domain).map_err(|e| e.to_string())?;
        let mut out = vec![0u8; 256 * 1024];
        let mut needed: usize = 0;
        let mut err = vec![0u8; 512];
        // SAFETY: `native` is the exported JNI native; every buffer outlives
        // the call, and the C side nul-terminates within `out_len`.
        let rc = unsafe {
            cordial_cookies_get_for_domain(
                native,
                cls.as_ptr(),
                dom.as_ptr(),
                out.as_mut_ptr() as *mut c_char,
                out.len(),
                &mut needed as *mut usize,
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };
        if rc != 0 {
            return Err(take_err(err));
        }
        out.truncate(needed);
        String::from_utf8(out)
            .map(Jar)
            .map_err(|_| "the engine's cookie jar was not UTF-8".to_string())
    }

    /// A static native taking `(boolean, String)` — `setTaskSchedulerBackgroundMode`.
    pub fn call_static_bool_string(
        native: *mut c_void,
        class_name: &str,
        flag: bool,
        text: &str,
    ) -> Result<(), String> {
        let cls = CString::new(class_name).map_err(|e| e.to_string())?;
        let t = CString::new(text).map_err(|e| e.to_string())?;
        let mut err = vec![0u8; 512];
        // SAFETY: `native` is the exported JNI native; every buffer outlives the call.
        let rc = unsafe {
            cordial_call_static_bool_string(
                native,
                cls.as_ptr(),
                if flag { 1 } else { 0 },
                t.as_ptr(),
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    /// `NativeSettingsInterface.nativeSetDeviceInfo(DeviceParams)`.
    pub fn set_device_info(native: *mut c_void, width: i32, height: i32) -> Result<(), String> {
        let mut err = vec![0u8; 512];
        // SAFETY: `native` is the exported JNI native; `err` outlives the call.
        let rc = unsafe {
            cordial_set_device_info(
                native,
                width,
                height,
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    /// `FlagJniInterface.nativeInitializeNativeFlags` — what `bootstrapTheApp`
    /// exists to reach. Without it the engine reports `onFlagsFailed` and stops.
    pub fn init_flags(native: *mut c_void, settings_json: &str) -> Result<(), String> {
        let json = CString::new(settings_json).map_err(|e| e.to_string())?;
        let mut err = vec![0u8; 512];
        // SAFETY: `native` is the exported JNI native; both buffers outlive the call.
        let rc = unsafe {
            cordial_init_flags(
                native,
                json.as_ptr(),
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    /// `NativeGLInterface.readLocalFlags()` — the offline counterpart to the
    /// network `ClientSettings` fetch. Not on the `ActivityNativeMain` chain
    /// Cordial drives (its only dex caller is a different startup path), so
    /// nothing else here calls it unless a caller in `load.rs` does.
    pub fn read_local_flags(native: *mut c_void) -> Result<(), String> {
        let mut err = vec![0u8; 512];
        // SAFETY: `native` is the exported JNI native; `err` is a live buffer.
        let rc =
            unsafe { cordial_read_local_flags(native, err.as_mut_ptr() as *mut c_char, err.len()) };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    /// A no-argument native on a named class. `nativeAppBridgeAppStart` is on
    /// `NativeAppBridgeInterface`, not `NativeGLInterface`.
    pub fn call_bare_on(native: *mut c_void, class_name: &str) -> Result<(), String> {
        let cls = CString::new(class_name).map_err(|e| e.to_string())?;
        let mut err = vec![0u8; 512];
        // SAFETY: `native` is the exported JNI native; buffers outlive the call.
        let rc = unsafe {
            cordial_appbridge_call_bare_cls(
                native,
                cls.as_ptr(),
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    /// `NativeGLInterface.nativeInitClientSettings(String, String, String)I` —
    /// what the real app calls after fetching client settings itself. Cordial
    /// *is* the host app in this architecture, so this is the legitimate
    /// interface, not a workaround. Returns the engine's own `int` result
    /// code, which is a better signal than anything printed to the log.
    pub fn init_client_settings(native: *mut c_void, a: &str, b: &str, c: &str) -> Result<i32, String> {
        let ca = CString::new(a).map_err(|e| e.to_string())?;
        let cb = CString::new(b).map_err(|e| e.to_string())?;
        let cc = CString::new(c).map_err(|e| e.to_string())?;
        let mut err = vec![0u8; 512];
        let mut out: c_int = 0;
        // SAFETY: `native` is the exported JNI native; all buffers outlive the call.
        let rc = unsafe {
            cordial_init_client_settings(
                native,
                ca.as_ptr(),
                cb.as_ptr(),
                cc.as_ptr(),
                &mut out as *mut c_int,
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };
        if rc == 0 { Ok(out) } else { Err(take_err(err)) }
    }

    /// `NativeGLInterface.nativeInitClientSettingsCachedCompressed(...)I` —
    /// hand the engine back the compressed flag cache it wrote itself.
    ///
    /// Cordial has only ever used the plain three-string form, so every launch
    /// has looked cold to the engine even with `flag_cache.dat` on disk beside
    /// it. Returns the engine's own `int`, on the same reasoning as
    /// [`init_client_settings`]: the result code is a better signal than the log.
    #[allow(clippy::too_many_arguments)]
    pub fn init_client_settings_cached_compressed(
        native: *mut c_void,
        data: &[u8],
        a: &str,
        b: &str,
        c: &str,
        when: i64,
        flag: bool,
    ) -> Result<i32, String> {
        let ca = CString::new(a).map_err(|e| e.to_string())?;
        let cb = CString::new(b).map_err(|e| e.to_string())?;
        let cc = CString::new(c).map_err(|e| e.to_string())?;
        let mut err = vec![0u8; 512];
        let mut out: c_int = 0;
        // SAFETY: `native` is the exported JNI native; every buffer outlives the
        // call, and `data` is copied into a Java array on the other side.
        let rc = unsafe {
            cordial_init_client_settings_cached_compressed(
                native,
                data.as_ptr(),
                data.len(),
                ca.as_ptr(),
                cb.as_ptr(),
                cc.as_ptr(),
                when,
                c_int::from(flag),
                &mut out as *mut c_int,
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };
        if rc == 0 { Ok(out) } else { Err(take_err(err)) }
    }

    /// Tell the framework layer the real window size.
    ///
    /// The C++ setter behind this had no `extern "C"` and therefore no caller,
    /// so `DisplayMetrics`, the User-Agent resolution fields and the
    /// `AConfiguration` screen size have all been reporting the compiled
    /// 1280x720 regardless of the window. Call it as soon as the window's
    /// geometry is known, and again whenever it changes.
    /// The desktop's dark/light preference, as Android's `uiMode` night field.
    ///
    /// Cordial hardcoded "night: no" and Roblox believed it, which is why the
    /// client stayed light however the desktop was set. Anything above zero
    /// reports night mode on; `-1` means nobody said and leaves the old
    /// behaviour, because a runtime started without the shell has no better
    /// answer and guessing dark would be as wrong as guessing light.
    pub fn set_ui_mode_night(night: i32) {
        // SAFETY: stores an int into an atomic on the C++ side; no ownership.
        unsafe { cordial_set_ui_mode_night(night as c_int) }
    }

    pub fn set_display_size(width: i32, height: i32) {
        // SAFETY: writes two ints behind a mutex-free but single-threaded
        // startup path, the same one `set_init_params` already runs on.
        unsafe { cordial_set_display_size(width as c_int, height as c_int) }
    }

    /// `FlagJniInterface.nativeGetFInt(String, int)I` — ask the engine what a
    /// flag actually holds, rather than inferring it from behaviour.
    ///
    /// `fallback` comes back when the name is not a registered flag, so a
    /// sentinel separates "the engine has this set to 0" from "the engine has
    /// never heard of it". Cordial spent a session unable to tell those apart
    /// for `FLogNativeDM`; see docs/analysis/flag-init.md §22.
    pub fn get_fint(native: *mut c_void, name: &str, fallback: i32) -> Result<i32, String> {
        let cn = CString::new(name).map_err(|e| e.to_string())?;
        let mut err = vec![0u8; 512];
        let mut out: c_int = 0;
        // SAFETY: `native` is the exported JNI native; all buffers outlive the call.
        let rc = unsafe {
            cordial_get_fint(
                native,
                cn.as_ptr(),
                fallback as c_int,
                &mut out as *mut c_int,
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };
        if rc == 0 { Ok(out) } else { Err(take_err(err)) }
    }

    /// `NativeGLInterface.nativePostClientSettingsLoadedInitialization3(List)V`
    /// — the finishing step of the client-settings handshake, called with an
    /// empty `ArrayList`.
    pub fn post_client_settings_loaded(native: *mut c_void) -> Result<(), String> {
        let mut err = vec![0u8; 512];
        // SAFETY: as above.
        let rc = unsafe {
            cordial_post_client_settings_loaded(native, err.as_mut_ptr() as *mut c_char, err.len())
        };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    /// `MainGameActivity.nativePreloadFlagOverrides(String)V` — takes whatever
    /// JSON text is given and hands it straight through, so candidate shapes
    /// can be compared by their effect on the flags verdict / JNI trace.
    pub fn preload_flag_overrides(native: *mut c_void, json: &str) -> Result<(), String> {
        let cs = CString::new(json).map_err(|e| e.to_string())?;
        let mut err = vec![0u8; 512];
        // SAFETY: `native` is the exported JNI native; `cs`/`err` outlive the call.
        let rc = unsafe {
            cordial_preload_flag_overrides(
                native,
                cs.as_ptr(),
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    /// `NativeGLInterface.nativeAppBridgeV2InitWithParams` — the real app-bridge
    /// entry. The launcher Activity targets `ActivityNativeMain`, whose chain runs
    /// through here rather than through AGDK's `MainGameActivity`.
    pub fn appbridge_init(
        native: *mut c_void,
        assets: &str,
        width: i32,
        height: i32,
    ) -> Result<(), String> {
        let a = CString::new(assets).map_err(|e| e.to_string())?;
        let mut err = vec![0u8; 512];
        // SAFETY: `native` is the exported JNI native; `a` outlives the call.
        let rc = unsafe {
            cordial_appbridge_init(
                native,
                a.as_ptr(),
                width,
                height,
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    /// A `NativeGLInterface` native taking no arguments — `nativeAppBridgeStartLuaAppDM`.
    pub fn appbridge_call_bare(native: *mut c_void) -> Result<(), String> {
        let mut err = vec![0u8; 512];
        // SAFETY: as above.
        let rc = unsafe {
            cordial_appbridge_call_bare(native, err.as_mut_ptr() as *mut c_char, err.len())
        };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    /// `nativeAppBridgeV2StartAppWithParams` — the call that hands the engine
    /// its window. Everything before it is setup.
    /// `nativeAppBridgeV2UpdateSurfaceApp/GameWithPlatformParams`.
    ///
    /// Two calls Sober makes and Cordial did not — see `update_surface` in
    /// `native/init_params.cpp` for the measurement. `game` selects the
    /// three-argument form, which takes an Activity as well.
    pub fn appbridge_update_surface(
        native: *mut c_void,
        assets: &str,
        width: i32,
        height: i32,
        game: bool,
    ) -> Result<(), String> {
        let a = CString::new(assets).map_err(|e| e.to_string())?;
        let mut err = vec![0u8; 512];
        // SAFETY: `native` is the exported JNI native; `a` outlives the call.
        let rc = unsafe {
            let f = if game {
                cordial_appbridge_update_surface_game
            } else {
                cordial_appbridge_update_surface_app
            };
            f(native, a.as_ptr(), width, height, err.as_mut_ptr() as *mut c_char, err.len())
        };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    pub fn appbridge_start_app(
        native: *mut c_void,
        assets: &str,
        width: i32,
        height: i32,
    ) -> Result<(), String> {
        let a = CString::new(assets).map_err(|e| e.to_string())?;
        let mut err = vec![0u8; 512];
        // SAFETY: `native` is the exported JNI native; `a` outlives the call.
        let rc = unsafe {
            cordial_appbridge_start_app(
                native,
                a.as_ptr(),
                width,
                height,
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    /// One of `JNIActivityLifecycleCallbacks`' natives. The engine stores
    /// per-Activity context — including the JNI environment it later reaches
    /// through — as these fire.
    pub fn activity_lifecycle(native: *mut c_void, activity: &str) -> Result<(), String> {
        let a = CString::new(activity).map_err(|e| e.to_string())?;
        let mut err = vec![0u8; 512];
        // SAFETY: `native` is the exported JNI native; `a` outlives the call.
        let rc = unsafe {
            cordial_activity_lifecycle(
                native,
                a.as_ptr(),
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    /// A native taking nothing but the JNI pair — `nativeRetryInit`.
    pub fn call_bare(native: *mut c_void) -> Result<(), String> {
        let mut err = vec![0u8; 512];
        // SAFETY: as above.
        let rc = unsafe { cordial_call_bare(native, err.as_mut_ptr() as *mut c_char, err.len()) };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    pub fn initialize(
        native: *mut c_void,
        internal_path: &str,
        obb_path: &str,
        external_path: &str,
    ) -> Result<i64, String> {
        let internal = CString::new(internal_path).map_err(|e| e.to_string())?;
        let obb = CString::new(obb_path).map_err(|e| e.to_string())?;
        let external = CString::new(external_path).map_err(|e| e.to_string())?;
        let mut err = vec![0u8; 512];

        // SAFETY: `native` is libroblox's initializeNativeCode export; the paths
        // outlive the call. The shim takes the JNI environment from the VM
        // itself — Rust cannot name `jnivm::ENV` and must not pretend to.
        let handle = unsafe {
            cordial_game_activity_init(
                native,
                internal.as_ptr(),
                obb.as_ptr(),
                external.as_ptr(),
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };

        if handle == 0 {
            let end = err.iter().position(|&b| b == 0).unwrap_or(err.len());
            let msg = String::from_utf8_lossy(&err[..end]).into_owned();
            Err(if msg.is_empty() {
                "initializeNativeCode returned a null handle".into()
            } else {
                msg
            })
        } else {
            Ok(handle)
        }
    }

    extern "C" {
        fn cordial_game_activity_touch(
            handle: i64,
            action: c_int,
            x: f32,
            y: f32,
            button_state: c_int,
            action_button: c_int,
            event_time_ms: i64,
            down_time_ms: i64,
            consumed: *mut c_int,
            err: *mut c_char,
            err_len: usize,
        ) -> c_int;
        fn cordial_game_activity_touch_multi(
            handle: i64,
            action: c_int,
            contacts: *const TouchContact,
            count: c_int,
            event_time_ms: i64,
            down_time_ms: i64,
            consumed: *mut c_int,
            err: *mut c_char,
            err_len: usize,
        ) -> c_int;
        fn cordial_game_activity_scroll(
            handle: i64,
            x: f32,
            y: f32,
            hscroll: f32,
            vscroll: f32,
            event_time_ms: i64,
            consumed: *mut c_int,
            err: *mut c_char,
            err_len: usize,
        ) -> c_int;
        fn cordial_game_activity_key(
            handle: i64,
            down: c_int,
            key_code: c_int,
            scan_code: c_int,
            meta_state: c_int,
            repeat_count: c_int,
            unicode_char: c_int,
            event_time_ms: i64,
            down_time_ms: i64,
            consumed: *mut c_int,
            err: *mut c_char,
            err_len: usize,
        ) -> c_int;
    }

    /// Deliver a synthesised mouse pointer event through `onTouchEventNative`.
    ///
    /// `action` is an Android `MotionEvent.ACTION_*` constant. Returns
    /// `Ok(Some(consumed))` on success; `Ok(None)` if `onTouchEventNative` has
    /// not been registered yet, which happens for every call that arrives
    /// before `initializeNativeCode` has finished — a normal race during
    /// startup, not a failure. `x`/`y` are window-relative pixels, matching the
    /// `dpiScale = 1.0` Cordial reports in `PlatformParams`.
    #[allow(clippy::too_many_arguments)]
    pub fn touch(
        handle: i64,
        action: i32,
        x: f32,
        y: f32,
        button_state: i32,
        action_button: i32,
        event_time_ms: i64,
        down_time_ms: i64,
    ) -> Result<Option<bool>, String> {
        let mut err = vec![0u8; 512];
        let mut consumed: c_int = 0;
        // SAFETY: `handle` came from `initialize`; `err`/`consumed` are live.
        let rc = unsafe {
            cordial_game_activity_touch(
                handle,
                action,
                x,
                y,
                button_state,
                action_button,
                event_time_ms,
                down_time_ms,
                &mut consumed,
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };
        match rc {
            0 => Ok(Some(consumed != 0)),
            -2 => Ok(None),
            _ => Err(take_err(err)),
        }
    }

    /// One finger on the glass, as the C side receives it.
    ///
    /// Mirrors `CordialTouchContact` in `native/game_activity.cpp`; the two
    /// definitions are the same three words in the same order and have to stay
    /// that way. `id` is Android's stable *pointer id*, not the position of
    /// this contact in the slice — the slice's order is the pointer *index*,
    /// and the two stop agreeing the moment a finger that is not the last one
    /// lifts.
    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct TouchContact {
        pub id: i32,
        pub x: f32,
        pub y: f32,
    }

    /// Deliver a set of finger contacts through `onTouchEventNative`.
    ///
    /// `contacts` is every contact still on the glass in pointer-index order,
    /// including the one being lifted on an up action — Android reports a
    /// departing pointer in the array of the event that says it left. `action`
    /// is already packed with the pointer index for `ACTION_POINTER_DOWN`/`_UP`;
    /// `android::input` owns that arithmetic. See `touch`'s doc comment for the
    /// `Ok(None)` convention.
    pub fn touch_multi(
        handle: i64,
        action: i32,
        contacts: &[TouchContact],
        event_time_ms: i64,
        down_time_ms: i64,
    ) -> Result<Option<bool>, String> {
        if contacts.is_empty() {
            return Err("a touch event with no contacts".into());
        }
        let mut err = vec![0u8; 512];
        let mut consumed: c_int = 0;
        // SAFETY: `handle` came from `initialize`; `contacts` is a live slice
        // borrowed for the duration of the call and the C side neither keeps
        // nor frees it; `err`/`consumed` are live.
        let rc = unsafe {
            cordial_game_activity_touch_multi(
                handle,
                action,
                contacts.as_ptr(),
                contacts.len() as c_int,
                event_time_ms,
                down_time_ms,
                &mut consumed,
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };
        match rc {
            0 => Ok(Some(consumed != 0)),
            -2 => Ok(None),
            _ => Err(take_err(err)),
        }
    }

    /// Deliver a wheel movement through `onTouchEventNative` as ACTION_SCROLL.
    ///
    /// `hscroll`/`vscroll` are detents, positive right and positive away from
    /// the user, which is what `MotionEvent.AXIS_HSCROLL`/`AXIS_VSCROLL`
    /// document. See `touch`'s doc comment for the `Ok(None)` convention.
    pub fn scroll(
        handle: i64,
        x: f32,
        y: f32,
        hscroll: f32,
        vscroll: f32,
        event_time_ms: i64,
    ) -> Result<Option<bool>, String> {
        let mut err = vec![0u8; 512];
        let mut consumed: c_int = 0;
        // SAFETY: `handle` came from `initialize`; `err`/`consumed` are live.
        let rc = unsafe {
            cordial_game_activity_scroll(
                handle,
                x,
                y,
                hscroll,
                vscroll,
                event_time_ms,
                &mut consumed,
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };
        match rc {
            0 => Ok(Some(consumed != 0)),
            -2 => Ok(None),
            _ => Err(take_err(err)),
        }
    }

    /// Deliver a synthesised key event through `onKeyDownNative`/`onKeyUpNative`.
    ///
    /// See `touch`'s doc comment for the `Ok(None)` convention.
    #[allow(clippy::too_many_arguments)]
    pub fn key(
        handle: i64,
        down: bool,
        key_code: i32,
        scan_code: i32,
        meta_state: i32,
        repeat_count: i32,
        unicode_char: i32,
        event_time_ms: i64,
        down_time_ms: i64,
    ) -> Result<Option<bool>, String> {
        let mut err = vec![0u8; 512];
        let mut consumed: c_int = 0;
        // SAFETY: as above.
        let rc = unsafe {
            cordial_game_activity_key(
                handle,
                down as c_int,
                key_code,
                scan_code,
                meta_state,
                repeat_count,
                unicode_char,
                event_time_ms,
                down_time_ms,
                &mut consumed,
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };
        match rc {
            0 => Ok(Some(consumed != 0)),
            -2 => Ok(None),
            _ => Err(take_err(err)),
        }
    }

    extern "C" {
        fn cordial_game_activity_lifecycle(
            handle: i64,
            native_name: *const c_char,
            err: *mut c_char,
            err_len: usize,
        ) -> c_int;
        fn cordial_game_activity_text_input(
            handle: i64, text: *const c_char, sel_start: c_int, sel_end: c_int,
            err: *mut c_char, n: usize,
        ) -> c_int;
        fn cordial_input_key_event(
            f: *mut c_void, down: c_int, key_code: c_int, modifiers: c_int, is_repeat: c_int,
            err: *mut c_char, n: usize,
        ) -> c_int;
        fn cordial_game_activity_surface_resized(
            handle: i64, format: c_int, width: c_int, height: c_int,
            err: *mut c_char, n: usize,
        ) -> c_int;
        fn cordial_input_update_keyboard_size(
            f: *mut c_void, visible: c_int, x: c_int, y: c_int, w: c_int, h: c_int,
            err: *mut c_char, n: usize,
        ) -> c_int;
        fn cordial_input_sync_textbox(
            f: *mut c_void, text: *const c_char, cursor: c_int,
            err: *mut c_char, n: usize,
        ) -> c_int;
        fn cordial_input_pass_text(
            f: *mut c_void, which: i64, text: *const c_char, flag: c_int, cursor: c_int,
            err: *mut c_char, n: usize,
        ) -> c_int;
        fn cordial_input_mouse_move(
            f: *mut c_void, x: f32, y: f32, dx: f32, dy: f32,
            err: *mut c_char, n: usize,
        ) -> c_int;
        fn cordial_input_mouse_button(
            f: *mut c_void, x: f32, y: f32, down: c_int, button: c_int,
            err: *mut c_char, n: usize,
        ) -> c_int;
        fn cordial_input_mouse_wheel(
            f: *mut c_void, x: f32, y: f32, delta: f32,
            err: *mut c_char, n: usize,
        ) -> c_int;
        fn cordial_input_pass_input(
            f: *mut c_void, pointer_id: c_int, x: f32, y: f32, action: c_int,
            width: c_int, height: c_int, err: *mut c_char, n: usize,
        ) -> c_int;
        fn cordial_set_touchscreen_present(present: c_int);
        fn cordial_input_gamepad_connect(
            f: *mut c_void, id: c_int, gamepad_type: c_int,
            err: *mut c_char, n: usize,
        ) -> c_int;
        fn cordial_input_gamepad_disconnect(
            f: *mut c_void, id: c_int,
            err: *mut c_char, n: usize,
        ) -> c_int;
        fn cordial_input_gamepad_button(
            f: *mut c_void, id: c_int, key_code: c_int, action: c_int,
            err: *mut c_char, n: usize,
        ) -> c_int;
        fn cordial_input_gamepad_axis(
            f: *mut c_void, id: c_int, axis: c_int, x: f32, y: f32, z: f32,
            err: *mut c_char, n: usize,
        ) -> c_int;
        fn cordial_input_gamepad_supported_key(
            f: *mut c_void, id: c_int, key_code: c_int, supported: c_int, gamepad_type: c_int,
            err: *mut c_char, n: usize,
        ) -> c_int;
        fn cordial_input_gamepad_supported_motion(
            f: *mut c_void, id: c_int, axis: c_int, source: c_int, supported: c_int,
            gamepad_type: c_int, err: *mut c_char, n: usize,
        ) -> c_int;
        fn cordial_textbox_handle() -> i64;
        fn cordial_textbox_generation() -> c_int;
        fn cordial_textbox_text(buf: *mut c_char, n: c_int) -> c_int;
        fn cordial_registered_natives(
            class_name: *const c_char, out: *mut c_char, n: usize,
        ) -> c_int;
        fn cordial_textbox_info(out: *mut RawTextBoxInfo) -> c_int;
        fn cordial_textbox_info_now(
            f: *mut c_void, out: *mut RawTextBoxInfo, err: *mut c_char, n: usize,
        ) -> c_int;
        fn cordial_games_loaded() -> u32;
        fn cordial_last_place() -> i64;
        fn cordial_game_activity_window_focus(
            handle: i64,
            focused: c_int,
            err: *mut c_char,
            err_len: usize,
        ) -> c_int;
        fn cordial_init_storage_manager(
            native: *mut c_void,
            a: *const c_char,
            b: *const c_char,
            err: *mut c_char,
            err_len: usize,
        ) -> i32;
        fn cordial_game_activity_surface_redraw_needed(
            handle: i64,
            err: *mut c_char,
            err_len: usize,
        ) -> c_int;
        fn cordial_game_activity_set_input_connection(
            handle: i64,
            err: *mut c_char,
            err_len: usize,
        ) -> c_int;
        fn cordial_ime_state_generation() -> u32;
        fn cordial_ime_soft_keyboard_active() -> c_int;
        fn cordial_ime_state_text(buf: *mut c_char, n: c_int) -> c_int;
        fn cordial_ime_state_selection(start: *mut c_int, end: *mut c_int);
    }

    /// One `GameActivity` native shaped `(J)V` — `onPauseNative`,
    /// `onStopNative`, `onSurfaceDestroyedNative`, and `terminateNativeCode`
    /// at teardown. `Ok(None)` when `native_name` was never registered —
    /// treated as "did not happen" rather than an error, matching
    /// `touch`/`key`'s convention.
    pub fn lifecycle(handle: i64, native_name: &str) -> Result<Option<()>, String> {
        let name = CString::new(native_name).map_err(|e| e.to_string())?;
        let mut err = vec![0u8; 512];
        // SAFETY: `handle` came from `initialize`; `name`/`err` outlive the call.
        let rc = unsafe {
            cordial_game_activity_lifecycle(
                handle,
                name.as_ptr(),
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };
        match rc {
            0 => Ok(Some(())),
            -2 => Ok(None),
            _ => Err(take_err(err)),
        }
    }

    /// `onWindowFocusChangedNative(hasFocus)`. `start` already drives the
    /// `true` case inline at bring-up; this is for the `false` case Android
    /// sends immediately before `onPauseNative` when a run ends.
    /// `GameActivity.onTextInputEventNative` — the whole field contents.
    ///
    /// Android text fields receive state, not keystrokes, which is why keys
    /// alone left the login form's boxes empty.
    pub fn text_input(handle: i64, text: &str, sel_start: i32, sel_end: i32) -> Result<(), String> {
        let t = CString::new(text).map_err(|e| e.to_string())?;
        let mut err = vec![0u8; 512];
        // SAFETY: `t` and `err` outlive the call.
        let rc = unsafe {
            cordial_game_activity_text_input(
                handle, t.as_ptr(), sel_start, sel_end,
                err.as_mut_ptr() as *mut c_char, err.len(),
            )
        };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    /// `NativeGLInterface.nativePassKeyEvent` — Roblox's own keyboard path.
    pub fn pass_key_event(native: *mut c_void, down: bool, key_code: i32, modifiers: i32, is_repeat: bool) -> Result<(), String> {
        let mut err = vec![0u8; 512];
        // SAFETY: `err` outlives the call.
        let rc = unsafe {
            cordial_input_key_event(
                native, down as c_int, key_code, modifiers, is_repeat as c_int,
                err.as_mut_ptr() as *mut c_char, err.len(),
            )
        };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    /// Which text box the engine has focus in, learned from `showKeyboard`.
    /// `None` when nothing is focused — text must then not be sent at all,
    /// rather than sent to handle 0, which is what left the login form empty.
    pub fn focused_textbox() -> Option<i64> {
        // SAFETY: a plain atomic load on the C++ side.
        match unsafe { cordial_textbox_handle() } {
            0 => None,
            h => Some(h),
        }
    }

    /// Bumped on every focus change. Editing state keyed on this reseeds when
    /// focus moves, without comparing handles — a handle can be reused once a
    /// box is destroyed, so equal handles do not imply the same box.
    pub fn textbox_generation() -> u32 {
        // SAFETY: a plain atomic load on the C++ side.
        unsafe { cordial_textbox_generation() as u32 }
    }

    /// The spec for the editor that has to be drawn over the focused text box,
    /// as the engine handed it to `showKeyboard`.
    ///
    /// Layout must match `CordialTextBoxInfo` in `native/android_classes.cpp`
    /// field-for-field — this is a `#[repr(C)]` mirror, not a coincidence.
    ///
    /// The fourteen values are `NativeTextBoxInfo`'s constructor arguments,
    /// `(FFFFFZIIIIIIZZ)`. Slots that have been identified by watching real
    /// boxes on the Login screen carry a name; the rest keep their slot number,
    /// because the APK's declarations give the class's field list sorted by
    /// name and never say which order the constructor takes them in. The long
    /// comment on `CordialTextBoxInfo` in `native/android_classes.cpp` has the
    /// captured numbers and the argument from each of them; read it before
    /// renaming anything here.
    ///
    /// Do not assume a numbered slot means what the field list's reading order
    /// would suggest. That guess put `textColor` at slot 7 and it is at slot 8.
    ///
    /// **`x_alignment`/`y_alignment` (slots 6/7) are confirmed, not
    /// `INFERRED`, as of 2026-08-30** -- corroborated rather than guessed, by
    /// mocktail's `NativeTextBoxInfo` constructor
    /// (`~/Projects/mocktail/src/jnivm/jnivm.cc:4016-4024`, Apache-2.0), which
    /// declares the same six ints in the order `xAlignment, yAlignment,
    /// textColor, font, textInputType, returnKeyType`. That order is a fact
    /// about Roblox's platform API and is taken as one; the values this
    /// struct actually carries were captured from Cordial's own boxes and are
    /// not mocktail's. See the long comment on `CordialTextBoxInfo` in
    /// `native/android_classes.cpp` for the rest of the reasoning, and for
    /// what these names are evidence of: two positional readings of the same
    /// constructor agreeing, not a reflection of the real Java class.
    ///
    /// Every slot but the fifteenth carries a name as of 2026-09-04.
    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default, PartialEq)]
    pub struct RawTextBoxInfo {
        /// Left edge in surface pixels. Observed 470 for two boxes on a
        /// 1280-wide surface, one of them 340 wide, and 470 = (1280-340)/2.
        pub x: f32,
        /// Top edge in surface pixels; the coordinate that differed between two
        /// vertically stacked login fields.
        pub y: f32,
        pub width: f32,
        /// `INFERRED`. Observed 22 against a `font_size` of 16, and a 22px line
        /// box holding 16pt text is a text field where the reverse is not
        /// anything. Slots 3 and 4 are the two floats no observation has yet
        /// told apart.
        pub height: f32,
        /// `INFERRED`; see [`RawTextBoxInfo::height`].
        pub font_size: f32,
        /// Roblox's `TextBox.MultiLine`. Slot 5, settled 2026-09-04 by
        /// mocktail's constructor placing `textWrapped` at slot 13 -- which
        /// leaves 5 as the only candidate this project's own captures could
        /// not rule out. See `CordialTextBoxInfo` for both halves.
        pub multiline: i32,
        /// Roblox's `Enum.TextXAlignment`: `Left` = 0, `Center` = 1,
        /// `Right` = 2 -- Roblox's own published scripting-API ordinals.
        /// Confirmed as slot 6 by mocktail's constructor field order; see this
        /// struct's own doc comment.
        pub x_alignment: i32,
        /// Roblox's `Enum.TextYAlignment`: `Top` = 0, `Center` = 1,
        /// `Bottom` = 2. Confirmed as slot 7 alongside `x_alignment`.
        pub y_alignment: i32,
        /// Packed ARGB. Observed `0xffd5d5dd` on both login boxes, which is
        /// what identified this slot: nothing else in the class is a colour.
        pub text_color: i32,
        /// Roblox's font id for this box. Slot 9, which is what
        /// `editor_font::font_slot` already defaulted to.
        pub font: i32,
        /// Roblox's own input-type enum, not Android's `InputType`: the two
        /// login boxes reported 5 and 7, which are far too small for Android's
        /// packed class-plus-variation words. Slot 10.
        pub text_input_type: i32,
        /// Which action key the box asks for -- the two login boxes differed
        /// here, Next against Done. Slot 11.
        pub return_key_type: i32,
        /// Observed 1 on two single-line login fields, so this is *not*
        /// `multiline` — that is slot 5 or slot 13. Settling which wants a box
        /// that genuinely differs, such as an in-experience chat entry.
        /// Slot 12. Both login boxes reported 1.
        pub manual_focus_release: i32,
        /// Slot 13, which is what settles [`RawTextBoxInfo::multiline`].
        pub text_wrapped: i32,
        /// The fifteenth constructor slot. The dex signature is
        /// `(FFFFFZIIIIIIZZZ)` -- three trailing booleans, not two -- and
        /// omitting this one made the whole hook fail to match. Unnamed
        /// because nothing has established what it means, only that it exists.
        pub z14: i32,
    }

    /// The focused box's spec, or `None` when nothing is focused or the engine
    /// gave Cordial no `NativeTextBoxInfo` for it.
    ///
    /// `None` is not a zeroed box. A caller must not fall back to drawing an
    /// editor at the origin: an editor in the wrong place reads as a layout bug
    /// and hides the fact that the value never arrived.
    /// How many times the engine has reported a place finished loading.
    ///
    /// Both `gameLoadedCallback` and `onGameLoaded` bump it, because either
    /// means the join completed and different builds have been seen to call
    /// different ones. The join watchdog waits for this to move; it is a count
    /// rather than a flag so a second join in the same session is visible.
    pub fn games_loaded() -> u32 {
        // SAFETY: a plain atomic load on the C++ side.
        unsafe { cordial_games_loaded() }
    }

    /// The place id of the most recent load, or 0 if none has been reported.
    pub fn last_place() -> i64 {
        // SAFETY: a plain atomic load on the C++ side.
        unsafe { cordial_last_place() }
    }

    /// `NativeGLInterface.nativeGetTextBoxInfo()` — the focused box's geometry
    /// asked for now, rather than remembered from `showKeyboard`.
    ///
    /// **Not a replacement for [`focused_textbox_info`], a second opinion when
    /// that one is unusable.** The engine volunteers a spec at focus time and
    /// sometimes volunteers it too early: Roblox's search modal is focused with
    /// `w=0 h=0` and this call returns real geometry for the same box about a
    /// second later. It is also null for the whole of the sign-in page, so
    /// `Ok(None)` is an ordinary answer and not a failure.
    ///
    /// `native` is `Java_com_roblox_engine_jni_NativeGLInterface_nativeGetTextBoxInfo`,
    /// resolved by the loader. Calling this costs a JNI call and a Java object,
    /// so the caller owns the decision about how often — see `sync_text_overlay`.
    pub fn textbox_info_now(native: *mut c_void) -> Result<Option<RawTextBoxInfo>, String> {
        let mut info = RawTextBoxInfo::default();
        let mut err = vec![0u8; 512];
        // SAFETY: `info` is a live, fully initialised mirror of the C++ struct
        // and `err` outlives the call; both are only written into.
        let rc = unsafe {
            cordial_textbox_info_now(
                native,
                &mut info as *mut RawTextBoxInfo,
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };
        match rc {
            1 => Ok(Some(info)),
            0 => Ok(None),
            _ => Err(take_err(err)),
        }
    }

    pub fn focused_textbox_info() -> Option<RawTextBoxInfo> {
        let mut info = RawTextBoxInfo::default();
        // SAFETY: `info` is a live, fully initialised mirror of the C++ struct
        // and outlives the call, which only copies into it.
        let known = unsafe { cordial_textbox_info(&mut info as *mut RawTextBoxInfo) };
        if known == 0 { None } else { Some(info) }
    }

    /// Every native the engine has registered on `class_name`, and where each
    /// points, or an empty list when it has registered none.
    ///
    /// **This answers a question that has been argued rather than looked up.**
    /// A native registered through `RegisterNatives` never appears in `nm -D`,
    /// so an exported-symbol table says nothing about whether the engine drives
    /// a class -- and `docs/HANDOVER.md` concluded for weeks that voice chat's
    /// downlink "cannot be written" from exactly that absence. Cordial has
    /// depended on the distinction since `terminateNativeCode` (see
    /// `native/game_activity.cpp`) without being able to see it.
    ///
    /// The pointer is reported beside the name because "registered" and
    /// "registered to something real" are different claims.
    pub fn registered_natives(class_name: &str) -> String {
        let Ok(c) = CString::new(class_name) else {
            return String::new();
        };
        let mut buf = vec![0u8; 8192];
        // SAFETY: both buffers outlive the call and their lengths are passed.
        let n = unsafe {
            cordial_registered_natives(c.as_ptr(), buf.as_mut_ptr() as *mut c_char, buf.len())
        };
        if n < 0 {
            return String::new();
        }
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        String::from_utf8_lossy(&buf[..end]).into_owned()
    }

    /// The focused box's contents, as the engine reported them at focus time.
    pub fn textbox_text() -> String {
        let mut buf = vec![0u8; 4096];
        // SAFETY: `buf` is writable for its full length and outlives the call.
        let n = unsafe { cordial_textbox_text(buf.as_mut_ptr() as *mut c_char, buf.len() as c_int) };
        if n <= 0 {
            return String::new();
        }
        String::from_utf8_lossy(&buf[..n as usize]).into_owned()
    }

    /// Re-drive `onSurfaceChangedNative` after the host window is resized.
    pub fn surface_resized(handle: i64, format: i32, width: i32, height: i32) -> Result<(), String> {
        let mut err = vec![0u8; 512];
        // SAFETY: `err` outlives the call.
        let rc = unsafe {
            cordial_game_activity_surface_resized(
                handle, format, width, height,
                err.as_mut_ptr() as *mut c_char, err.len(),
            )
        };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    /// `NativeGLInterface.updateKeyboardSize` — tells the engine an editor is
    /// up. Without it the engine focuses a box but never starts capturing.
    pub fn update_keyboard_size(
        native: *mut c_void, visible: bool, x: i32, y: i32, w: i32, h: i32,
    ) -> Result<(), String> {
        let mut err = vec![0u8; 512];
        // SAFETY: `err` outlives the call.
        let rc = unsafe {
            cordial_input_update_keyboard_size(
                native, visible as c_int, x, y, w, h,
                err.as_mut_ptr() as *mut c_char, err.len(),
            )
        };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    /// `NativeGLInterface.syncTextboxTextAndCursorPosition2` — the per-keystroke
    /// text update. Takes no box handle: it applies to whatever has focus.
    pub fn sync_textbox(native: *mut c_void, text: &str, cursor: i32) -> Result<(), String> {
        let t = CString::new(text).map_err(|e| e.to_string())?;
        let mut err = vec![0u8; 512];
        // SAFETY: `t` and `err` outlive the call.
        let rc = unsafe {
            cordial_input_sync_textbox(
                native, t.as_ptr(), cursor,
                err.as_mut_ptr() as *mut c_char, err.len(),
            )
        };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    /// `NativeGLInterface.nativePassText` — text entered into a focused box.
    ///
    /// `which` is the handle from `showKeyboard`, which is how the engine knows
    /// which box the text belongs to.
    pub fn pass_text(native: *mut c_void, which: i64, text: &str, flag: bool, cursor: i32) -> Result<(), String> {
        let t = CString::new(text).map_err(|e| e.to_string())?;
        let mut err = vec![0u8; 512];
        // SAFETY: `t` and `err` outlive the call.
        let rc = unsafe {
            cordial_input_pass_text(
                native, which, t.as_ptr(), flag as c_int, cursor,
                err.as_mut_ptr() as *mut c_char, err.len(),
            )
        };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    /// `NativeInputInterface.nativePassMouseMove` — the path Roblox's interface
    /// actually reads, as distinct from AGDK's `onTouchEventNative`.
    pub fn pass_mouse_move(native: *mut c_void, x: f32, y: f32, dx: f32, dy: f32) -> Result<(), String> {
        let mut err = vec![0u8; 512];
        // SAFETY: `native` is the exported JNI native; `err` outlives the call.
        let rc = unsafe {
            cordial_input_mouse_move(native, x, y, dx, dy, err.as_mut_ptr() as *mut c_char, err.len())
        };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    /// `NativeInputInterface.nativePassMouseButton`.
    pub fn pass_mouse_button(native: *mut c_void, x: f32, y: f32, down: bool, button: i32) -> Result<(), String> {
        let mut err = vec![0u8; 512];
        // SAFETY: as above.
        let rc = unsafe {
            cordial_input_mouse_button(
                native, x, y, if down { 1 } else { 0 }, button,
                err.as_mut_ptr() as *mut c_char, err.len(),
            )
        };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    /// `NativeInputInterface.nativePassMouseWheel(F,F,F)` — the wheel's
    /// equivalent of [`pass_mouse_button`], and the call Cordial had never
    /// made. `delta` is in detents, positive away from the user.
    pub fn pass_mouse_wheel(native: *mut c_void, x: f32, y: f32, delta: f32) -> Result<(), String> {
        let mut err = vec![0u8; 512];
        // SAFETY: as above.
        let rc = unsafe {
            cordial_input_mouse_wheel(native, x, y, delta, err.as_mut_ptr() as *mut c_char, err.len())
        };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    /// Tell `PlatformParams`/`Configuration` whether this host has a
    /// touchscreen, before the engine is initialised and asks.
    ///
    /// `android::input::report_touchscreen` is the only caller and owns the
    /// policy — the seat's answer, and what `CORDIAL_INPUT_TOUCH` and
    /// `CORDIAL_NO_TOUCH` do to it. This is only the wire. Nothing on the C
    /// side reads it after startup; see `cordial_set_touchscreen_present` in
    /// `native/init_params.cpp` for why that is ordering rather than policy.
    pub fn set_touchscreen_present(present: bool) {
        // SAFETY: a plain store into a C++ `std::atomic<int>`; no pointers, no
        // allocation, and safe to call from any thread at any time.
        unsafe { cordial_set_touchscreen_present(if present { 1 } else { 0 }) }
    }

    /// `NativeInputInterface.nativePassInput(I,F,F,I,I,I)` — one finger, one
    /// call.
    ///
    /// The descriptor is read out of this build's dex; the three `action`
    /// values are `INFERRED` from mocktail. See `cordial_input_pass_input` in
    /// `native/game_activity.cpp` for both, and `android::input::TOUCH_*` for
    /// the constants themselves.
    pub fn pass_input(
        native: *mut c_void,
        pointer_id: i32,
        x: f32,
        y: f32,
        action: i32,
        width: i32,
        height: i32,
    ) -> Result<(), String> {
        let mut err = vec![0u8; 512];
        // SAFETY: as above.
        let rc = unsafe {
            cordial_input_pass_input(
                native, pointer_id, x, y, action, width, height,
                err.as_mut_ptr() as *mut c_char, err.len(),
            )
        };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    // `NativeInputInterface`'s six gamepad natives. Descriptors read from the
    // shipping APK's dex, not guessed; what the integer arguments *mean* is
    // INFERRED and is argued out in `native/game_activity.cpp` beside each
    // trampoline, and in `cordial_runtime::android::gamepad`. Callers should go
    // through that module rather than these directly -- it is what holds the
    // all-or-nothing resolution gate and the off switch.

    /// `nativeGamepadConnectEventWithGamepadType(I id, I gamepadType)`.
    ///
    /// **This build ships no type-less connect**, so there is no way to announce
    /// a pad without naming a type, and no evidence available here says what the
    /// ordinals are. See `android::gamepad::gamepad_type`.
    pub fn gamepad_connect(native: *mut c_void, id: i32, gamepad_type: i32) -> Result<(), String> {
        let mut err = vec![0u8; 512];
        // SAFETY: `native` is the exported JNI native; `err` outlives the call.
        let rc = unsafe {
            cordial_input_gamepad_connect(
                native, id, gamepad_type,
                err.as_mut_ptr() as *mut c_char, err.len(),
            )
        };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    /// `nativeGamepadDisconnectEvent(I id)` — no type, because the engine kept
    /// the one it was given at connect.
    pub fn gamepad_disconnect(native: *mut c_void, id: i32) -> Result<(), String> {
        let mut err = vec![0u8; 512];
        // SAFETY: as above.
        let rc = unsafe {
            cordial_input_gamepad_disconnect(native, id, err.as_mut_ptr() as *mut c_char, err.len())
        };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    /// `nativeGamepadButtonEvent(I id, I keyCode, I action)`.
    ///
    /// `key_code` is read as an Android `KeyEvent.KEYCODE_BUTTON_*` and `action`
    /// as `ACTION_DOWN`/`ACTION_UP`. INFERRED from the Android platform contract
    /// the Java caller would have been working to; nothing observed it.
    pub fn gamepad_button(native: *mut c_void, id: i32, key_code: i32, action: i32) -> Result<(), String> {
        let mut err = vec![0u8; 512];
        // SAFETY: as above.
        let rc = unsafe {
            cordial_input_gamepad_button(
                native, id, key_code, action,
                err.as_mut_ptr() as *mut c_char, err.len(),
            )
        };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    /// `nativeGamepadAxisEvent(I id, I axis, F x, F y, F z)`.
    ///
    /// Three floats read as a `Vector3`, which is what Roblox's Lua
    /// `InputObject.Position` is for a thumbstick. INFERRED with no control
    /// behind it — the TV-remote family has no axis method to difference against.
    pub fn gamepad_axis(native: *mut c_void, id: i32, axis: i32, x: f32, y: f32, z: f32) -> Result<(), String> {
        let mut err = vec![0u8; 512];
        // SAFETY: as above.
        let rc = unsafe {
            cordial_input_gamepad_axis(
                native, id, axis, x, y, z,
                err.as_mut_ptr() as *mut c_char, err.len(),
            )
        };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    /// `nativeSetGamepadSupportedKeyWithGamepadType(I id, I keyCode, Z supported, I gamepadType)`
    /// — one call per button, before any button event.
    pub fn gamepad_supported_key(
        native: *mut c_void, id: i32, key_code: i32, supported: bool, gamepad_type: i32,
    ) -> Result<(), String> {
        let mut err = vec![0u8; 512];
        // SAFETY: as above.
        let rc = unsafe {
            cordial_input_gamepad_supported_key(
                native, id, key_code, if supported { 1 } else { 0 }, gamepad_type,
                err.as_mut_ptr() as *mut c_char, err.len(),
            )
        };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    /// `nativeSetGamepadSupportedMotionWithGamepadType(I id, I axis, I source, Z supported, I gamepadType)`.
    ///
    /// The middle pair is read as Android's `(axis, source)` motion-range key.
    /// The least established of the six: `(IIIZI)` has one more `int` than the
    /// key variant and nothing to difference it against.
    pub fn gamepad_supported_motion(
        native: *mut c_void, id: i32, axis: i32, source: i32, supported: bool, gamepad_type: i32,
    ) -> Result<(), String> {
        let mut err = vec![0u8; 512];
        // SAFETY: as above.
        let rc = unsafe {
            cordial_input_gamepad_supported_motion(
                native, id, axis, source, if supported { 1 } else { 0 }, gamepad_type,
                err.as_mut_ptr() as *mut c_char, err.len(),
            )
        };
        if rc == 0 { Ok(()) } else { Err(take_err(err)) }
    }

    pub fn window_focus(handle: i64, focused: bool) -> Result<Option<()>, String> {
        let mut err = vec![0u8; 512];
        // SAFETY: `handle` came from `initialize`; `err` is a live buffer.
        let rc = unsafe {
            cordial_game_activity_window_focus(
                handle,
                if focused { 1 } else { 0 },
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };
        match rc {
            0 => Ok(Some(())),
            -2 => Ok(None),
            _ => Err(take_err(err)),
        }
    }

    /// `onSurfaceRedrawNeededNative` — the "repaint now" nudge, driven from
    /// X11 `Expose`.
    pub fn surface_redraw_needed(handle: i64) -> Result<Option<()>, String> {
        let mut err = vec![0u8; 512];
        // SAFETY: as above.
        let rc = unsafe {
            cordial_game_activity_surface_redraw_needed(
                handle,
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };
        match rc {
            0 => Ok(Some(())),
            -2 => Ok(None),
            _ => Err(take_err(err)),
        }
    }

    /// `GameActivity.setInputConnectionNative` — hands the engine the
    /// `InputConnection` it will later call `setState`/`setSoftKeyboardActive`/
    /// `restartInput` on. Meant to be driven once, early (see the call site in
    /// `load.rs`), not per frame — a second call would construct and register a
    /// second `InputConnection` C++ side, but the engine keeps calling back on
    /// whichever one it saw first, so nothing after the first call would ever
    /// be reached anyway.
    pub fn set_input_connection(handle: i64) -> Result<Option<()>, String> {
        let mut err = vec![0u8; 512];
        // SAFETY: `err` outlives the call.
        let rc = unsafe {
            cordial_game_activity_set_input_connection(
                handle,
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };
        match rc {
            0 => Ok(Some(())),
            -2 => Ok(None),
            _ => Err(take_err(err)),
        }
    }

    /// Bumped on every `InputConnection.setState`/`restartInput` the engine has
    /// made — the outbound half of the IME contract, as distinct from
    /// [`textbox_generation`]'s focus-change counter driven by `showKeyboard`.
    pub fn ime_state_generation() -> u32 {
        // SAFETY: a plain atomic load on the C++ side.
        unsafe { cordial_ime_state_generation() }
    }

    /// Whether the engine last asked for a soft keyboard via
    /// `InputConnection.setSoftKeyboardActive`. Not currently wired to
    /// anything — `updateKeyboardSize` remains the outbound acknowledgement
    /// path (see `android::input`'s `keyboard_report_enabled` doc) — kept
    /// available for whichever future change replaces it, so that decision
    /// does not also have to rediscover how to read this flag.
    pub fn ime_soft_keyboard_active() -> bool {
        // SAFETY: a plain atomic load on the C++ side.
        unsafe { cordial_ime_soft_keyboard_active() != 0 }
    }

    /// The text `InputConnection.setState` last reported, i.e. what the engine
    /// itself currently believes the focused field contains — the "real"
    /// contents `android::input`'s reseed should prefer over `showKeyboard`'s
    /// one-shot byte array once a `setState` has actually been observed for
    /// the current focus.
    pub fn ime_state_text() -> String {
        let mut buf = vec![0u8; 4096];
        // SAFETY: `buf` is writable for its full length and outlives the call.
        let n = unsafe { cordial_ime_state_text(buf.as_mut_ptr() as *mut c_char, buf.len() as c_int) };
        if n <= 0 {
            return String::new();
        }
        String::from_utf8_lossy(&buf[..n as usize]).into_owned()
    }

    /// The selection `InputConnection.setState` last reported, as a
    /// `(start, end)` char-offset pair. Roblox's own text boxes are
    /// single-caret in practice, so `android::input` collapses this to
    /// `end` for its own caret; the pair is returned as-is in case a future
    /// caller needs a real selection range.
    pub fn ime_state_selection() -> (i32, i32) {
        let mut start: c_int = 0;
        let mut end: c_int = 0;
        // SAFETY: `start`/`end` are live out-parameters for the call.
        unsafe { cordial_ime_state_selection(&mut start, &mut end) };
        (start, end)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        extern "C" {
            fn cordial_textbox_focused(
                handle: i64,
                text: *const c_char,
                info: *const RawTextBoxInfo,
            );
            fn cordial_textbox_blurred();
            #[allow(clippy::too_many_arguments)]
            fn cordial_textbox_test_focus(
                handle: i64, text: *const c_char,
                f0: f32, f1: f32, f2: f32, f3: f32, f4: f32, multiline: c_int,
                x_alignment: c_int, y_alignment: c_int, text_color: c_int,
                font: c_int, text_input_type: c_int, return_key_type: c_int,
                manual_focus_release: c_int, text_wrapped: c_int, z14: c_int,
            );
        }

        /// The fifteen values arriving in the slots they were sent to, with
        /// C++ naming the members on its side and Rust naming them again on
        /// this one. That is what makes the two layouts have to agree: a test
        /// that handed a Rust-built struct to `cordial_textbox_focused` and
        /// read it back passes with the mirror shifted by a field, because
        /// nothing between the two ever looks inside.
        ///
        /// Every value is distinct so a drift of one slot cannot land on an
        /// equal number and go unseen.
        ///
        /// This says nothing about which slot means what — that is the engine's
        /// to answer, not this crate's.
        ///
        /// One test rather than several: the focused box is process-wide state,
        /// and the test harness runs tests on threads, so two of these would
        /// race each other into an intermittent failure of exactly the kind
        /// this repository has already spent time chasing.
        #[test]
        fn textbox_info_arrives_slot_for_slot() {
            let text = CString::new("hello").expect("no interior NUL");
            // SAFETY: `text` outlives the call, which only reads it.
            unsafe {
                cordial_textbox_test_focus(
                    42, text.as_ptr(),
                    1.5, 2.5, 3.5, 4.5, 5.5, 1,
                    6, 7, 8, 9, 10, 11,
                    0, 1, 1,
                )
            };

            assert_eq!(focused_textbox(), Some(42));
            assert_eq!(
                focused_textbox_info(),
                Some(RawTextBoxInfo {
                    x: 1.5, y: 2.5, width: 3.5, height: 4.5, font_size: 5.5,
                    multiline: 1,
                    x_alignment: 6, y_alignment: 7, text_color: 8,
                    font: 9, text_input_type: 10, return_key_type: 11,
                    manual_focus_release: 0, text_wrapped: 1, z14: 1,
                })
            );

            // SAFETY: no arguments; clears the global the assertions above set.
            unsafe { cordial_textbox_blurred() };
            assert_eq!(focused_textbox(), None);
            // Blur has to drop the spec too, or a caller keeps an editor up
            // over a box that no longer has focus.
            assert_eq!(focused_textbox_info(), None);

            // A focus the engine supplied no spec for reports none rather than
            // a zeroed box, and does not resurrect the previous box's numbers.
            // Nothing has run `NativeTextBoxInfo.<init>` in this process, so
            // the last-built fallback is empty too.
            // SAFETY: `text` outlives the call; a null spec is the documented
            // "the engine gave us nothing" case.
            unsafe { cordial_textbox_focused(7, text.as_ptr(), std::ptr::null()) };
            assert_eq!(focused_textbox(), Some(7));
            assert_eq!(focused_textbox_info(), None);

            // SAFETY: no arguments.
            unsafe { cordial_textbox_blurred() };
        }
    }
}

/// The accessibility mirror `native/accessibility.cpp` builds from whatever
/// `AccessibilityNodeInfo`/`AccessibilityManager`/`AccessibilityEvent` calls
/// Roblox's engine makes over JNI. Kept as its own top-level module rather
/// than folded into [`game_activity`]: unlike everything else there, nothing
/// here is on the render/input critical path, and
/// `crates/cordial-runtime/src/android/accessibility.rs` is the only caller,
/// so a clean boundary keeps that file's own header comment about what is and
/// is not verified from getting lost among touch/key/IME plumbing.
pub mod accessibility {
    use std::ffi::{c_char, c_int, CString};

    /// Layout must match `CordialA11yNode` in `native/accessibility.cpp`
    /// field-for-field — this is a `#[repr(C)]` mirror, not a coincidence.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct RawNode {
        pub id: u32,
        pub class_name: [c_char; 128],
        pub text: [c_char; 256],
        pub content_description: [c_char; 256],
        pub left: i32,
        pub top: i32,
        pub right: i32,
        pub bottom: i32,
        pub state: u32,
        pub actions: [i32; 16],
        pub action_count: u32,
    }

    impl Default for RawNode {
        fn default() -> Self {
            // SAFETY: an all-zero `RawNode` is a valid value for every field
            // — the char arrays are NUL already, every integer's zero is a
            // legitimate zero, not an uninitialised trap representation.
            unsafe { std::mem::zeroed() }
        }
    }

    fn cstr_field(buf: &[c_char]) -> String {
        // SAFETY: `buf` is a NUL-terminated `snprintf` target on the C++
        // side, always at least one byte, so a NUL is guaranteed to exist
        // within `buf`'s own length.
        let bytes: &[u8] = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, buf.len()) };
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        String::from_utf8_lossy(&bytes[..end]).into_owned()
    }

    /// A [`RawNode`], decoded into owned Rust types. What
    /// `crates/cordial-runtime/src/android/accessibility.rs` actually works
    /// with — the raw struct exists only to cross the FFI boundary cheaply.
    #[derive(Clone, Debug, Default)]
    pub struct Node {
        pub id: u32,
        pub class_name: String,
        pub text: String,
        pub content_description: String,
        pub left: i32,
        pub top: i32,
        pub right: i32,
        pub bottom: i32,
        pub state: u32,
        pub actions: Vec<i32>,
    }

    impl From<RawNode> for Node {
        fn from(r: RawNode) -> Self {
            let n = r.action_count as usize;
            let n = n.min(r.actions.len());
            Node {
                id: r.id,
                class_name: cstr_field(&r.class_name),
                text: cstr_field(&r.text),
                content_description: cstr_field(&r.content_description),
                left: r.left,
                top: r.top,
                right: r.right,
                bottom: r.bottom,
                state: r.state,
                actions: r.actions[..n].to_vec(),
            }
        }
    }

    /// State bits, mirrored from the `NodeStateBit` enum in
    /// `native/accessibility.cpp` — this file's own bit layout, translated
    /// into real AT-SPI `StateType` ordinals on the `accessibility.rs` side.
    pub mod state_bit {
        pub const CHECKABLE: u32 = 1 << 0;
        pub const CHECKED: u32 = 1 << 1;
        pub const CLICKABLE: u32 = 1 << 2;
        pub const ENABLED: u32 = 1 << 3;
        pub const FOCUSABLE: u32 = 1 << 4;
        pub const FOCUSED: u32 = 1 << 5;
        pub const LONG_CLICKABLE: u32 = 1 << 6;
        pub const PASSWORD: u32 = 1 << 7;
        pub const SCROLLABLE: u32 = 1 << 8;
        pub const SELECTED: u32 = 1 << 9;
        pub const VISIBLE_TO_USER: u32 = 1 << 10;
    }

    extern "C" {
        fn cordial_accessibility_set_bridge_connected(connected: c_int);
        fn cordial_accessibility_snapshot(out: *mut RawNode, max: usize) -> usize;
        fn cordial_accessibility_node_count() -> usize;
        fn cordial_accessibility_generation() -> u32;
        fn cordial_accessibility_next_event(
            event_type: *mut c_int,
            class_name_buf: *mut c_char,
            cn_len: c_int,
            text_buf: *mut c_char,
            text_len: c_int,
        ) -> c_int;
        fn cordial_accessibility_test_seed_node(
            class_name: *const c_char,
            text: *const c_char,
            content_description: *const c_char,
            left: c_int,
            top: c_int,
            right: c_int,
            bottom: c_int,
            state: u32,
        ) -> u32;
        fn cordial_accessibility_test_clear();
    }

    /// Tell `AccessibilityManager.isEnabled()` whether to answer true.
    /// `accessibility.rs` calls this once, after it knows whether it managed
    /// to attach to the AT-SPI bus — see that file for why the gate lives on
    /// this side of the boundary rather than the engine blocking a JNI call
    /// on a D-Bus round-trip.
    pub fn set_bridge_connected(connected: bool) {
        // SAFETY: a plain atomic store on the C++ side, no aliasing concerns.
        unsafe { cordial_accessibility_set_bridge_connected(connected as c_int) };
    }

    /// Every node currently in the mirror. Not cheap — copies the whole
    /// registry — so callers should gate this on [`generation`] having
    /// changed rather than call it on a tight poll loop.
    pub fn snapshot() -> Vec<Node> {
        // SAFETY: a first call establishes how many nodes exist; a second,
        // sized to match, copies them. A node added between the two calls is
        // simply missed until the next poll, which is fine for a bridge whose
        // whole design is "poll and diff", not "exactly once".
        let count = unsafe { cordial_accessibility_node_count() };
        if count == 0 {
            return Vec::new();
        }
        let mut buf = vec![RawNode::default(); count];
        let written = unsafe { cordial_accessibility_snapshot(buf.as_mut_ptr(), buf.len()) };
        buf.truncate(written);
        buf.into_iter().map(Node::from).collect()
    }

    /// Bumped on every node add/change/recycle. Cheap to poll — a plain
    /// atomic load on the C++ side — which is the point: a caller can spin a
    /// loop on this without the cost `snapshot()` would carry.
    pub fn generation() -> u32 {
        // SAFETY: a plain atomic load on the C++ side.
        unsafe { cordial_accessibility_generation() }
    }

    /// One pending `AccessibilityManager.sendAccessibilityEvent` call, if any
    /// is queued. `(event_type, class_name, text)`. Callers should drain in a
    /// loop until this returns `None` — the engine can queue events faster
    /// than a poll loop drains them.
    pub fn next_event() -> Option<(i32, String, String)> {
        let mut event_type: c_int = 0;
        let mut cn = vec![0u8; 256];
        let mut text = vec![0u8; 512];
        // SAFETY: `cn`/`text` are writable for their full length and outlive
        // the call; `event_type` is a live out-parameter.
        let got = unsafe {
            cordial_accessibility_next_event(
                &mut event_type,
                cn.as_mut_ptr() as *mut c_char,
                cn.len() as c_int,
                text.as_mut_ptr() as *mut c_char,
                text.len() as c_int,
            )
        };
        if got == 0 {
            return None;
        }
        let cn_end = cn.iter().position(|&b| b == 0).unwrap_or(cn.len());
        let text_end = text.iter().position(|&b| b == 0).unwrap_or(text.len());
        Some((
            event_type as i32,
            String::from_utf8_lossy(&cn[..cn_end]).into_owned(),
            String::from_utf8_lossy(&text[..text_end]).into_owned(),
        ))
    }

    /// Inject one synthetic node, bypassing JNI entirely. Test-only — see
    /// `cordial_accessibility_test_seed_node`'s own doc comment in
    /// `native/accessibility.cpp` for why this exists and why it is not a
    /// second way for Roblox to reach the registry. Returns the assigned id.
    pub fn test_seed_node(
        class_name: &str,
        text: &str,
        content_description: &str,
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
        state: u32,
    ) -> u32 {
        let class_name = CString::new(class_name).unwrap_or_default();
        let text = CString::new(text).unwrap_or_default();
        let content_description = CString::new(content_description).unwrap_or_default();
        // SAFETY: all three `CString`s outlive the call.
        unsafe {
            cordial_accessibility_test_seed_node(
                class_name.as_ptr(),
                text.as_ptr(),
                content_description.as_ptr(),
                left,
                top,
                right,
                bottom,
                state,
            )
        }
    }

    /// Drop every node, seeded or (in principle) real. Test-only.
    pub fn test_clear() {
        // SAFETY: no arguments, no aliasing concerns.
        unsafe { cordial_accessibility_test_clear() };
    }
}
