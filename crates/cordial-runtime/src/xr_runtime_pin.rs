//! Keeps the host's OpenXR runtime library loaded once an instance exists.
//!
//! Joining a game after leaving one makes the Quest engine destroy its
//! `XrInstance` and create another (docs/vr/play-button.md). The host loader
//! unloads the runtime library when the last instance goes and loads it again
//! for the next one, at another address, and the engine goes on calling
//! `xrRequestDisplayRefreshRateFB` through the pointer it fetched under the
//! first instance: measured on Monado, the client faulted at the old mapping's
//! base plus `0x3e080`, which is `oxr_xrRequestDisplayRefreshRateFB` in
//! `libopenxr_monado.so`. The specification makes a fetched pointer good for
//! its own instance only, so this is the engine's assumption, not the
//! loader's mistake; that the Quest's runtime survives it, by never being
//! unloaded, is INFERRED.
//!
//! Pinning the library with `RTLD_NODELETE` keeps those addresses mapped and
//! the code at them the runtime's own. That an old instance's pointer then
//! does the right thing for the new one holds for runtimes whose entry points
//! dispatch on the handle passed rather than on the instance they were
//! fetched under, which Monado's do (`oxr_api_*`); for any other runtime it is
//! INFERRED. `CORDIAL_NO_XR_PIN=1` is the control.

use std::ffi::{c_char, c_void, CStr};
use std::sync::atomic::{AtomicBool, Ordering};

#[repr(C)]
struct DlInfo {
    fname: *const c_char,
    fbase: *mut c_void,
    sname: *const c_char,
    saddr: *mut c_void,
}

unsafe extern "C" {
    fn dladdr(addr: *mut c_void, info: *mut DlInfo) -> i32;
    fn dlopen(filename: *const c_char, flags: i32) -> *mut c_void;
}

const RTLD_NOW: i32 = 2;
const RTLD_NOLOAD: i32 = 4;
const RTLD_NODELETE: i32 = 0x1000;

static PINNED: AtomicBool = AtomicBool::new(false);

/// Commands a loader hands straight to the runtime: the first whose pointer
/// lies outside the loader names the runtime's library.
const PROBES: [&CStr; 3] = [c"xrCreateSession", c"xrGetSystem", c"xrPollEvent"];

/// After `xrCreateInstance(...) -> instance`, with the host loader's own
/// `xrGetInstanceProcAddr`.
pub(crate) fn pin(instance: u64, loader_gipa: Option<usize>) {
    if PINNED.load(Ordering::Acquire) || std::env::var_os("CORDIAL_NO_XR_PIN").is_some() {
        return;
    }
    let Some(gipa) = loader_gipa else { return };
    // SAFETY: the loader's documented `xrGetInstanceProcAddr` signature.
    let gipa: extern "C" fn(u64, *const c_char, *mut usize) -> i32 = unsafe { std::mem::transmute(gipa) };
    let loader = library_of(gipa as usize);
    for name in PROBES {
        let mut f = 0usize;
        if gipa(instance, name.as_ptr(), &mut f) != 0 || f == 0 {
            continue;
        }
        let Some(lib) = library_of(f) else { continue };
        if Some(&lib) == loader.as_ref() {
            continue;
        }
        let Ok(c) = std::ffi::CString::new(lib.clone()) else { return };
        // SAFETY: a library already loaded (NOLOAD), named by dladdr.
        let h = unsafe { dlopen(c.as_ptr(), RTLD_NOW | RTLD_NOLOAD | RTLD_NODELETE) };
        if h.is_null() {
            eprintln!("[guest] openxr: could not keep the runtime {lib} loaded; a second instance may find the \
                       engine calling into an unloaded library");
        } else {
            PINNED.store(true, Ordering::Release);
            eprintln!("[guest] openxr: keeping the runtime {lib} loaded for the process, since the engine \
                       reuses entry points across instances");
        }
        return;
    }
    eprintln!("[guest] openxr: the runtime's library could not be told from the loader's; not pinned");
}

fn library_of(addr: usize) -> Option<String> {
    let mut info = DlInfo {
        fname: std::ptr::null(),
        fbase: std::ptr::null_mut(),
        sname: std::ptr::null(),
        saddr: std::ptr::null_mut(),
    };
    // SAFETY: dladdr only reads the address's mapping.
    if unsafe { dladdr(addr as *mut c_void, &mut info) } == 0 || info.fname.is_null() {
        return None;
    }
    // SAFETY: dladdr's file name, NUL-terminated, owned by the dynamic linker.
    Some(unsafe { CStr::from_ptr(info.fname) }.to_string_lossy().into_owned())
}
