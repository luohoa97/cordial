//! The arm64 guest's `libopenxr_loader.so` (docs/vr/dynarmic-design.md §3.2, M6).
//!
//! The Quest engine imports its core OpenXR commands from the loader and
//! fetches the rest through `xrGetInstanceProcAddr`. Under dynarmic every one
//! of them is a stub whose arguments move from AAPCS64 registers into a SysV
//! call to the **host's own loader** (`libopenxr_loader.so.1`), which finds
//! the runtime the ordinary desktop way, `XR_RUNTIME_JSON` or the active
//! runtime file. The signatures are generated from Khronos's `xr.xml`
//! (`tools/vr/gen-guest-xr.py`), and so is the probe the layout-diff gate
//! compiles: every struct `openxr.h` and `openxr_platform.h` declare for
//! Android and Vulkan is the same on aarch64-android and x86-64 Linux
//! (`tests::layout_gate`), so plain data crosses unchanged, including the
//! nested pointers in `xrEndFrame`'s layer arrays -- identity mapping (§2).
//!
//! What cannot cross unchanged is written out below, one command at a time:
//!
//! * `xrInitializeLoaderKHR`. Android's loader needs the `JavaVM` and the
//!   activity; a desktop loader needs nothing, so accepting it is the truthful
//!   answer. The Android structure is recorded and never handed to the host.
//! * `xrCreateInstance`. `XR_KHR_android_create_instance` and its
//!   `XrInstanceCreateInfoAndroidKHR` link are removed from a copy, the same
//!   substitution `vulkan.rs` makes for `VK_KHR_android_surface`, and a
//!   debug-utils messenger's callback is put behind a host-to-guest entry.
//!   Every other extension is passed through, so one the runtime lacks fails
//!   the call with `XR_ERROR_EXTENSION_NOT_PRESENT`, honestly.
//! * `XR_KHR_vulkan_enable2`'s `pfnGetInstanceProcAddr`, which the engine
//!   fills with the guest `vkGetInstanceProcAddr` stub, becomes Cordial's
//!   native one, so the runtime's `VkInstance` and `VkDevice` are created
//!   through the same interposition as the engine's own Vulkan (Android
//!   surface translated) and are handles the guest's Vulkan stubs work with.
//!   The nested `VkInstanceCreateInfo`/`VkDeviceCreateInfo` chains and
//!   allocator go through `guest_vk`'s translation.
//! * Frames are counted and timed at `xrEndFrame`, since an XR engine never
//!   presents, and `cordial_screenshot` reads the projection layer's
//!   left-eye image as the engine releases it (`capture`).
//!
//! No guest memory is written except where a command's own contract writes
//! it (output structs and arrays the engine passed).

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::ffi::{c_char, c_void, CStr};
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use cordial_guest::{Fault, Handler, Ret, Runtime, Ty};

#[path = "guest_xr_table.rs"]
#[allow(dead_code)]
mod table;

#[path = "guest_xr_input.rs"]
mod input;

const XR_SUCCESS: i32 = 0;
const XR_ERROR_VALIDATION_FAILURE: i32 = -1;
const XR_ERROR_FUNCTION_UNSUPPORTED: i32 = -7;
const XR_ERROR_HANDLE_INVALID: i32 = -12;
const XR_ERROR_RUNTIME_UNAVAILABLE: i32 = -51;

const XR_TYPE_EVENT_DATA_SESSION_STATE_CHANGED: u32 = 18;
const XR_TYPE_SWAPCHAIN_CREATE_INFO: u32 = 9;
const XR_TYPE_COMPOSITION_LAYER_PROJECTION: u32 = 35;
const XR_TYPE_INSTANCE_CREATE_INFO_ANDROID_KHR: u32 = 1_000_008_000;
const XR_TYPE_GRAPHICS_BINDING_VULKAN_KHR: u32 = 1_000_025_000;
const XR_TYPE_LOADER_INIT_INFO_ANDROID_KHR: u32 = 1_000_089_000;
const ANDROID_CREATE_INSTANCE: &[u8] = b"XR_KHR_android_create_instance";

fn unsupported(thunk: &str, why: String) -> Fault {
    Fault::Unsupported { thunk: thunk.to_owned(), why }
}

/// Offset of `member` in `strukt`, from the generated table (x86-64, equal to
/// aarch64 by the gate). A name missing from the table is a bug here, caught
/// by `tests::every_offset_used_is_generated`.
fn off(strukt: &str, member: &str) -> usize {
    table::OFFSETS.iter().find(|r| r.0 == strukt && r.1 == member)
        .unwrap_or_else(|| panic!("{strukt}.{member} is not in the generated offsets")).2
}

fn signature(name: &str) -> Option<(&'static str, &'static [Ty], Ret)> {
    static S: OnceLock<HashMap<&'static str, (&'static [Ty], Ret)>> = OnceLock::new();
    S.get_or_init(|| table::XR.iter().map(|&(n, a, r)| (n, (a, r))).collect())
        .get_key_value(name).map(|(&n, &(a, r))| (n, a, r))
}

fn type_size(ty: u32) -> Option<(&'static str, usize)> {
    table::TYPE_SIZES.binary_search_by_key(&ty, |r| r.0).ok().map(|i| (table::TYPE_SIZES[i].1, table::TYPE_SIZES[i].2))
}

// ------------------------------------------------------------ host loader

struct HostXr {
    lib: usize,
}

extern "C" {
    // The host's dlopen, as in `android::vulkan`: the guest's own dlopen is
    // the bionic linker's, which is what this module answers for.
    fn dlopen(filename: *const c_char, flags: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    #[link_name = "gettid"]
    fn libc_gettid() -> i32;
}

/// The host loader, opened once. `CORDIAL_NO_OPENXR=1` makes it absent, which
/// is the control: every command then answers as M5's virtual loader did.
fn host() -> Option<&'static HostXr> {
    static H: OnceLock<Option<HostXr>> = OnceLock::new();
    H.get_or_init(|| {
        if std::env::var_os("CORDIAL_NO_OPENXR").is_some() {
            eprintln!("[guest] openxr: CORDIAL_NO_OPENXR is set; no host loader");
            return None;
        }
        for name in [c"libopenxr_loader.so.1", c"libopenxr_loader.so"] {
            // SAFETY: literal sonames; the handle is never closed.
            let lib = unsafe { dlopen(name.as_ptr(), 2) };
            if lib.is_null() {
                continue;
            }
            // SAFETY: the loader's documented export.
            if unsafe { dlsym(lib, c"xrGetInstanceProcAddr".as_ptr()) }.is_null() {
                continue;
            }
            eprintln!("[guest] openxr: host loader {} (XR_RUNTIME_JSON={})", name.to_string_lossy(),
                      std::env::var("XR_RUNTIME_JSON").unwrap_or_else(|_| "unset, so the active runtime".into()));
            return Some(HostXr { lib: lib as usize });
        }
        eprintln!("[guest] openxr: no host libopenxr_loader.so.1; answering as a loader with no runtime");
        None
    }).as_ref()
}

/// The host function for a command the engine imports directly: the
/// loader's own export, which dispatches through the instance.
fn host_export(name: &str) -> Option<usize> {
    let h = host()?;
    let c = std::ffi::CString::new(name).ok()?;
    // SAFETY: the open loader, a NUL-terminated name.
    let p = unsafe { dlsym(h.lib as *mut c_void, c.as_ptr()) };
    (!p.is_null()).then_some(p as usize)
}

// ------------------------------------------------------------- stub making

/// The guest stub for a directly imported OpenXR command. The host function
/// is looked up at the first call, so linking the engine never opens the
/// host loader.
pub fn import(name: &str) -> Handler {
    let name: &'static str = Box::leak(name.to_owned().into_boxed_str());
    let resolved: OnceLock<Option<Handler>> = OnceLock::new();
    Box::new(move |c| {
        let h = resolved.get_or_init(|| {
            if name == "xrInitializeLoaderKHR" {
                return Some(initialize_loader());
            }
            let host = host_export(name)?;
            Some(handler(c.runtime(), name, host))
        });
        match h {
            Some(h) => h(c),
            None => {
                no_runtime(c, name);
                Ok(())
            }
        }
    })
}

/// M5's answer, kept for a host with no loader: the entry points that need
/// no handle report `XR_ERROR_RUNTIME_UNAVAILABLE`, and everything else,
/// which needs a handle nothing could have issued, `XR_ERROR_HANDLE_INVALID`.
fn no_runtime(c: &cordial_guest::Call, name: &str) {
    let r = match name {
        "xrCreateInstance" | "xrEnumerateInstanceExtensionProperties" | "xrEnumerateApiLayerProperties" => {
            XR_ERROR_RUNTIME_UNAVAILABLE
        }
        "xrGetInstanceProcAddr" => {
            if c.x(2) != 0 {
                // SAFETY: the guest's PFN_xrVoidFunction*, cleared on failure as
                // the specification requires.
                unsafe { (c.x(2) as *mut u64).write(0) };
            }
            if c.x(0) == 0 { XR_ERROR_RUNTIME_UNAVAILABLE } else { XR_ERROR_HANDLE_INVALID }
        }
        _ => XR_ERROR_HANDLE_INVALID,
    };
    say_once(format!("{name} -> {} (no host loader)", result_name(r)));
    c.set_x(0, r as i64 as u64);
}

/// A stub for command `name` over host function `host`, made once per pair:
/// what `xrGetInstanceProcAddr` hands the guest.
fn wrap(rt: &Arc<Runtime>, name: &str, host: usize) -> Option<u64> {
    static MADE: Mutex<Option<HashMap<(String, usize), u64>>> = Mutex::new(None);
    let key = (name.to_owned(), host);
    if let Some(&s) = MADE.lock().unwrap().get_or_insert_with(HashMap::new).get(&key) {
        return Some(s);
    }
    let (static_name, _, _) = signature(name)?;
    let stub = rt.register(static_name, handler(rt, static_name, host));
    MADE.lock().unwrap().get_or_insert_with(HashMap::new).insert(key, stub);
    Some(stub)
}

/// The handler for one command over its host function.
fn handler(_rt: &Arc<Runtime>, name: &'static str, host: usize) -> Handler {
    let Some((_, args, ret)) = signature(name) else {
        let why = table::REFUSED.iter().find(|r| r.0 == name).map_or("not in the generated table", |r| r.1);
        let why = why.to_owned();
        return Box::new(move |_| Err(unsupported(name, why.clone())));
    };
    match name {
        "xrGetInstanceProcAddr" => get_instance_proc_addr(host),
        "xrInitializeLoaderKHR" => initialize_loader(),
        "xrCreateInstance" => create_instance(host),
        "xrCreateDebugUtilsMessengerEXT" => create_messenger(host),
        "xrCreateVulkanInstanceKHR" => create_vulkan(name, host, "XrVulkanInstanceCreateInfoKHR"),
        "xrCreateVulkanDeviceKHR" => create_vulkan(name, host, "XrVulkanDeviceCreateInfoKHR"),
        "xrCreateSwapchain" => create_swapchain(host),
        "xrApplyHapticFeedback" => apply_haptic(host),
        "xrStopHapticFeedback" => stop_haptic(host),
        _ => {
            let after = observer(name);
            let logged = input::hook(name);
            let said: Mutex<BTreeSet<i32>> = Mutex::new(BTreeSet::new());
            Box::new(move |c| {
                let mut vals = cordial_guest::collect(c, args);
                let _keep = after.before.as_ref().and_then(|before| before(&mut vals));
                // SAFETY: the host loader's function for `name`, whose SysV
                // signature `args` describes, with the guest's values; every
                // pointer is identity-mapped and every struct laid out alike.
                let out = unsafe { cordial_guest::invoke(name, host as *const c_void, args, &vals) }?;
                cordial_guest::write_ret(c, ret, &out);
                let r = out.rax as i32;
                if let Some(obs) = &after.after {
                    obs(&vals, r);
                }
                if let Some(obs) = &logged {
                    obs(&vals, r);
                }
                if said.lock().unwrap().insert(r) && !after.quiet_success || r < 0 && said.lock().unwrap().len() < 8 {
                    say(format!("{name} -> {}", result_name(r)));
                }
                Ok(())
            })
        }
    }
}

type Observe = Box<dyn Fn(&[u64], i32) + Send + Sync>;
/// May replace an argument, returning whatever the replacement points into so
/// it outlives the call.
type Before = Box<dyn Fn(&mut [u64]) -> Option<Box<[u64]>> + Send + Sync>;

/// What the bridge notes around a pass-through command: frame timing, the
/// swapchains and session the capture needs, and session state.
struct Observer {
    before: Option<Before>,
    after: Option<Observe>,
    /// Said at the first result only, not once per distinct success code:
    /// the per-frame commands.
    quiet_success: bool,
}

fn observer(name: &str) -> Observer {
    let none = Observer { before: None, after: None, quiet_success: false };
    let after = |f: Observe| Observer { before: None, after: Some(f), quiet_success: false };
    match name {
        "xrEnumerateInstanceExtensionProperties" => after(Box::new(|v, r| {
            if r == XR_SUCCESS && v[2] != 0 && v[3] != 0 {
                // SAFETY: the guest's count and XrExtensionProperties array,
                // just filled by the loader.
                let n = unsafe { *(v[2] as *const u32) } as usize;
                let stride = type_size(2).map_or(152, |t| t.1);
                // SAFETY: `n` entries of `stride` bytes, extensionName at 16.
                let names: Vec<String> = (0..n).map(|i| unsafe {
                    CStr::from_ptr((v[3] as usize + i * stride + 16) as *const c_char).to_string_lossy().into_owned()
                }).collect();
                say_once(format!("the runtime reports {n} instance extensions: {}", names.join(" ")));
            }
        })),
        "xrGetSystemProperties" => after(Box::new(|v, r| {
            if r == XR_SUCCESS {
                // SAFETY: the XrSystemProperties the guest passed, just filled.
                let name = unsafe { CStr::from_ptr((v[2] as usize + off("XrSystemProperties", "systemName")) as *const c_char) };
                // SAFETY: as above.
                let vendor = unsafe { *((v[2] as usize + off("XrSystemProperties", "vendorId")) as *const u32) };
                let g = v[2] as usize + off("XrSystemProperties", "graphicsProperties");
                // SAFETY: the XrSystemGraphicsProperties inside it: three uint32_t.
                let (w, h, layers) = unsafe { (*(g as *const u32), *((g + 4) as *const u32), *((g + 8) as *const u32)) };
                say_once(format!("system \"{}\", vendor {vendor:#x}, max swapchain {w}x{h}, {layers} layers",
                                 name.to_string_lossy()));
            }
        })),
        "xrCreateSession" => after(Box::new(|v, r| {
            // SAFETY: the guest's XrSessionCreateInfo chain, read only.
            unsafe { note_session(v[1], v[2], r) };
        })),
        "xrDestroySwapchain" => after(Box::new(|v, _| {
            XR.lock().unwrap().swapchains.remove(&v[0]);
        })),
        "xrEnumerateSwapchainImages" => after(Box::new(|v, r| {
            if r == XR_SUCCESS && v[3] != 0 {
                // SAFETY: the count and XrSwapchainImageVulkanKHR array the
                // runtime just filled.
                unsafe { note_images(v[0], *(v[2] as *const u32), v[3]) };
            }
        })),
        "xrAcquireSwapchainImage" => Observer {
            before: None,
            after: Some(Box::new(|v, r| {
                if r == XR_SUCCESS && v[2] != 0 {
                    // SAFETY: the index the runtime just wrote.
                    let i = unsafe { *(v[2] as *const u32) };
                    if let Some(s) = XR.lock().unwrap().swapchains.get_mut(&v[0]) {
                        s.acquired.push_back(i);
                    }
                }
            })),
            quiet_success: true,
        },
        "xrReleaseSwapchainImage" => Observer {
            before: Some(Box::new(|v| {
                release(v[0]);
                None
            })),
            after: None,
            quiet_success: true,
        },
        "xrWaitFrame" => Observer {
            before: None,
            after: Some(Box::new(|v, r| {
                if r == XR_SUCCESS && v[2] != 0 {
                    // SAFETY: the XrFrameState the runtime just filled.
                    let p = unsafe { *((v[2] as usize + off("XrFrameState", "predictedDisplayPeriod")) as *const i64) };
                    LAST_PERIOD.store(p as u64, Ordering::Relaxed);
                }
            })),
            quiet_success: true,
        },
        "xrEndFrame" => Observer {
            // SAFETY: the guest's XrFrameEndInfo, read only.
            before: Some(Box::new(|v| {
                unsafe { end_frame(v[1]) };
                None
            })),
            after: None,
            quiet_success: true,
        },
        "xrPollEvent" => Observer {
            before: None,
            after: Some(Box::new(|v, r| {
                if r == XR_SUCCESS && v[1] != 0 {
                    // SAFETY: the XrEventDataBuffer the runtime just filled.
                    unsafe { note_event(v[1]) };
                }
            })),
            quiet_success: true,
        },
        "xrRequestDisplayRefreshRateFB" => Observer {
            before: None,
            after: Some(Box::new(|v, r| {
                say_once(format!("xrRequestDisplayRefreshRateFB({} Hz) -> {}", f32::from_bits(v[1] as u32), result_name(r)));
            })),
            quiet_success: true,
        },
        "xrGetDisplayRefreshRateFB" => after(Box::new(|v, r| {
            if r == XR_SUCCESS && v[1] != 0 {
                // SAFETY: the float the runtime just wrote.
                say_once(format!("xrGetDisplayRefreshRateFB -> {} Hz", unsafe { *(v[1] as *const f32) }));
            }
        })),
        "xrBeginFrame" | "xrLocateViews" | "xrLocateSpace" | "xrSyncActions" | "xrGetActionStateBoolean"
        | "xrGetActionStateFloat" | "xrWaitSwapchainImage" | "xrGetActionStatePose" | "xrApplyHapticFeedback"
        | "xrStopHapticFeedback" | "xrGetActionStateVector2f" => Observer { quiet_success: true, ..none },
        _ => none,
    }
}

// --------------------------------------------------------- special commands

/// `xrGetInstanceProcAddr(instance, name, function)`: the host loader's
/// answer, handed back as a stub made from the command's generated
/// signature. `xrInitializeLoaderKHR` is answered here (see
/// `initialize_loader`) and never asked of the host.
fn get_instance_proc_addr(host: usize) -> Handler {
    Box::new(move |c| {
        let (inst, name_ptr, out) = (c.x(0), c.x(1), c.x(2) as *mut u64);
        if name_ptr == 0 || out.is_null() {
            c.set_x(0, XR_ERROR_VALIDATION_FAILURE as i64 as u64);
            return Ok(());
        }
        // SAFETY: the guest's NUL-terminated command name.
        let name = unsafe { CStr::from_ptr(name_ptr as *const c_char) }.to_string_lossy().into_owned();
        let (r, stub) = if name == "xrInitializeLoaderKHR" {
            (XR_SUCCESS, c.runtime().register("xrInitializeLoaderKHR", initialize_loader()))
        } else {
            let mut f = 0u64;
            // SAFETY: the host loader's xrGetInstanceProcAddr, the guest's
            // handle and name, and a host out-pointer.
            let hr = unsafe {
                cordial_guest::invoke("xrGetInstanceProcAddr", host as *const c_void, &[Ty::U64, Ty::Ptr, Ty::Ptr],
                                      &[inst, name_ptr, &mut f as *mut u64 as u64])
            }?.rax as i32;
            if hr < 0 || f == 0 {
                (if hr < 0 { hr } else { XR_ERROR_FUNCTION_UNSUPPORTED }, 0)
            } else {
                match wrap(c.runtime(), &name, f as usize) {
                    Some(s) => (hr, s),
                    None => {
                        say_once(format!("xrGetInstanceProcAddr(\"{name}\"): the runtime has it, but it is not \
                                          in the generated table; answered XR_ERROR_FUNCTION_UNSUPPORTED"));
                        (XR_ERROR_FUNCTION_UNSUPPORTED, 0)
                    }
                }
            }
        };
        // SAFETY: the guest's PFN_xrVoidFunction*; null on failure, as the
        // specification requires.
        unsafe { out.write(stub) };
        say_once(format!("xrGetInstanceProcAddr(instance {inst:#x}, \"{name}\") -> {}", result_name(r)));
        c.set_x(0, r as i64 as u64);
        Ok(())
    })
}

/// `xrInitializeLoaderKHR(const XrLoaderInitInfoBaseHeaderKHR*)`. Android's
/// loader needs the `JavaVM` and the application context to find the runtime
/// broker; the desktop loader this bridges to needs no initialisation at all,
/// so success is the truthful answer. The structure is recorded, not passed.
fn initialize_loader() -> Handler {
    Box::new(|c| {
        let info = c.x(0);
        let r = if info == 0 {
            XR_ERROR_VALIDATION_FAILURE
        } else {
            // SAFETY: the guest's structure, read only.
            let ty = unsafe { *(info as *const u32) };
            if ty == XR_TYPE_LOADER_INIT_INFO_ANDROID_KHR {
                // SAFETY: as above; an XrLoaderInitInfoAndroidKHR.
                let (vm, ctx) = unsafe {
                    (*((info as usize + off("XrLoaderInitInfoAndroidKHR", "applicationVM")) as *const u64),
                     *((info as usize + off("XrLoaderInitInfoAndroidKHR", "applicationContext")) as *const u64))
                };
                say(format!("xrInitializeLoaderKHR(XrLoaderInitInfoAndroidKHR: applicationVM {vm:#x}, \
                             applicationContext {ctx:#x}) -> XR_SUCCESS; the desktop loader needs no \
                             initialisation, and the Android structure is not passed to it"));
            } else {
                say(format!("xrInitializeLoaderKHR(type {ty}) -> XR_SUCCESS; the desktop loader needs no initialisation"));
            }
            XR_SUCCESS
        };
        c.set_x(0, r as i64 as u64);
        Ok(())
    })
}

/// Host copies of a `next` chain's links, each relinked to the next copy,
/// with the links of type `strip` left out and any callback member put behind
/// a host-to-guest entry. The top structure is always copied, so the caller
/// can edit it. Every link must be a type the headers declare, since its size
/// is needed; anything else refuses by name.
///
/// # Safety
///
/// `top` is a guest structure whose chain is well formed.
unsafe fn copy_chain(rt: &Arc<Runtime>, cmd: &str, top: u64, strip: &[u32], keep: &mut Vec<Box<[u64]>>)
                     -> Result<(u64, Vec<u32>), Fault> {
    let mut nodes = Vec::new();
    let mut stripped = Vec::new();
    let mut p = top;
    while p != 0 {
        if nodes.len() + stripped.len() > 256 {
            return Err(unsupported(cmd, "a next chain longer than 256 links, or a cycle".into()));
        }
        // SAFETY: a link of the guest's chain: type at 0, next at 8.
        let ty = unsafe { *(p as *const u32) };
        if p != top && strip.contains(&ty) {
            stripped.push(ty);
        } else {
            nodes.push(p);
        }
        // SAFETY: as above.
        p = unsafe { *((p + 8) as *const u64) };
    }
    let mut copies: Vec<Box<[u64]>> = Vec::with_capacity(nodes.len());
    for &n in &nodes {
        // SAFETY: as above.
        let ty = unsafe { *(n as *const u32) };
        let (sname, size) = type_size(ty)
            .ok_or_else(|| unsupported(cmd, format!("type {ty} in the next chain is not in openxr.h or openxr_platform.h")))?;
        let mut b = vec![0u64; size.div_ceil(8)].into_boxed_slice();
        // SAFETY: `size` bytes of the guest's structure, laid out as the host's.
        unsafe { std::ptr::copy_nonoverlapping(n as *const u8, b.as_mut_ptr().cast::<u8>(), size) };
        for &(_, _, moff, member, pfn) in table::PFN_MEMBERS.iter().filter(|r| r.1 == Some(ty)) {
            if pfn == "PFN_vkGetInstanceProcAddr" && n == top {
                continue; // create_vulkan's own to replace
            }
            if pfn != "PFN_xrDebugUtilsMessengerCallbackEXT" {
                return Err(unsupported(cmd, format!("{sname}.{member} in a next chain")));
            }
            b[moff / 8] = callback_entry(rt, pfn, b[moff / 8])?;
        }
        copies.push(b);
    }
    for i in 0..copies.len() {
        copies[i][1] = if i + 1 < copies.len() { copies[i + 1].as_ptr() as u64 } else { 0 };
    }
    let head = copies[0].as_ptr() as u64;
    keep.extend(copies);
    Ok((head, stripped))
}

fn callback_entry(rt: &Arc<Runtime>, pfn: &str, pc: u64) -> Result<u64, Fault> {
    static ENTRIES: Mutex<Option<HashMap<u64, u64>>> = Mutex::new(None);
    if pc == 0 {
        return Ok(0);
    }
    let mut m = ENTRIES.lock().unwrap();
    let m = m.get_or_insert_with(HashMap::new);
    if let Some(&e) = m.get(&pc) {
        return Ok(e);
    }
    let &(_, args, ret) = table::CALLBACKS.iter().find(|r| r.0 == pfn)
        .ok_or_else(|| unsupported(pfn, "no generated signature".into()))?;
    let e = cordial_guest::host_entry(rt, pfn, pc, args.to_vec(), ret, None).map_err(|why| unsupported(pfn, why))? as u64;
    m.insert(pc, e);
    Ok(e)
}

/// The strings of a guest `const char* const*` array.
///
/// # Safety
///
/// `arr` holds `n` pointers to NUL-terminated strings.
unsafe fn strings(arr: u64, n: usize) -> Vec<(u64, String)> {
    (0..n).map(|i| {
        // SAFETY: as the caller promises.
        let p = unsafe { *((arr as usize + i * 8) as *const u64) };
        // SAFETY: as the caller promises.
        (p, unsafe { CStr::from_ptr(p as *const c_char) }.to_string_lossy().into_owned())
    }).collect()
}

/// `xrCreateInstance(const XrInstanceCreateInfo*, XrInstance*)`.
fn create_instance(host: usize) -> Handler {
    Box::new(move |c| {
        let (info, out) = (c.x(0), c.x(1));
        if info == 0 {
            c.set_x(0, XR_ERROR_VALIDATION_FAILURE as i64 as u64);
            return Ok(());
        }
        let mut keep = Vec::new();
        // SAFETY: the guest's create info.
        let (copy, stripped) = unsafe { copy_chain(c.runtime(), "xrCreateInstance", info, &[XR_TYPE_INSTANCE_CREATE_INFO_ANDROID_KHR], &mut keep) }?;
        let (n_off, a_off) = (off("XrInstanceCreateInfo", "enabledExtensionCount"), off("XrInstanceCreateInfo", "enabledExtensionNames"));
        let app = info as usize + off("XrInstanceCreateInfo", "applicationInfo");
        // SAFETY: the guest's structure, read only; XrApplicationInfo is
        // applicationName[128], applicationVersion, engineName[128],
        // engineVersion, apiVersion (the gate checks the enclosing offsets).
        let (app_name, engine, api) = unsafe {
            (CStr::from_ptr(app as *const c_char).to_string_lossy().into_owned(),
             CStr::from_ptr((app + 132) as *const c_char).to_string_lossy().into_owned(),
             *((app + 264) as *const u64))
        };
        // SAFETY: as above.
        let (n, arr) = unsafe { (*((info as usize + n_off) as *const u32) as usize, *((info as usize + a_off) as *const u64)) };
        // SAFETY: the guest's extension-name array of `n` strings.
        let names = unsafe { strings(arr, n) };
        let kept: Vec<u64> = names.iter().filter(|(_, s)| s.as_bytes() != ANDROID_CREATE_INSTANCE).map(|(p, _)| *p).collect();
        let kept = kept.into_boxed_slice();
        let hc = copy as *mut u8;
        // SAFETY: the host copy, at least sizeof(XrInstanceCreateInfo) long.
        unsafe {
            *(hc.add(n_off) as *mut u32) = kept.len() as u32;
            *(hc.add(a_off) as *mut u64) = kept.as_ptr() as u64;
        }
        say(format!("xrCreateInstance: application \"{app_name}\", engine \"{engine}\", API {}.{}.{}, extensions [{}]; \
                     passed on without XR_KHR_android_create_instance{}",
                    api >> 48, (api >> 32) & 0xffff, api & 0xffff_ffff,
                    names.iter().map(|(_, s)| s.as_str()).collect::<Vec<_>>().join(" "),
                    if stripped.is_empty() { "" } else { " and its XrInstanceCreateInfoAndroidKHR link" }));
        // SAFETY: the host loader's xrCreateInstance, a host copy that lives
        // in `keep`/`kept` for the call, and the guest's out-pointer.
        let r = unsafe { cordial_guest::invoke("xrCreateInstance", host as *const c_void, &[Ty::Ptr, Ty::Ptr], &[copy, out]) }?.rax as i32;
        // SAFETY: the instance the runtime just wrote.
        let inst = if r == XR_SUCCESS && out != 0 { unsafe { *(out as *const u64) } } else { 0 };
        say(format!("xrCreateInstance -> {} (instance {inst:#x})", result_name(r)));
        if inst != 0 {
            crate::xr_runtime_pin::pin(inst, host_export("xrGetInstanceProcAddr"));
        }
        drop(kept);
        drop(keep);
        c.set_x(0, r as i64 as u64);
        Ok(())
    })
}

// ------------------------------------------------------------ swapchains
//
// The engine asks for `VK_FORMAT_R8G8B8A8_UNORM` (37) and never calls
// `xrEnumerateSwapchainFormats`, on Monado and on WiVRn alike, so the
// runtime's preference order never reaches it. OpenXR reads a UNORM
// swapchain as linear and sRGB-encodes it for the display. The engine's
// bytes are already display-referred (design §9.7: the left eye blitted
// straight to Cordial's window is the right colour), so a runtime that
// follows the specification encodes them twice, which is the washed-out
// picture in Monado's preview and in the headset.
//
// So the runtime's swapchain is made as the sRGB twin of what the engine
// asked for -- a format the runtime lists -- with
// `XR_SWAPCHAIN_USAGE_MUTABLE_FORMAT_BIT`, which makes the image
// `VK_IMAGE_CREATE_MUTABLE_FORMAT_BIT` so the engine's own UNORM views of it
// stay valid. Written through those views the bytes land unchanged; what
// changes is only what the runtime is told they mean, and that is now true.
// `CORDIAL_XR_SWAPCHAIN_AS_ASKED=1` passes the request through unchanged: the
// control.

const XR_SWAPCHAIN_USAGE_COLOR_ATTACHMENT_BIT: u64 = 0x1;
const XR_SWAPCHAIN_USAGE_MUTABLE_FORMAT_BIT: u64 = 0x40;

fn vk_format_name(f: i64) -> &'static str {
    match f {
        37 => "R8G8B8A8_UNORM", 43 => "R8G8B8A8_SRGB", 44 => "B8G8R8A8_UNORM", 50 => "B8G8R8A8_SRGB",
        64 => "A2B10G10R10_UNORM_PACK32", 97 => "R16G16B16A16_SFLOAT", 91 => "R16G16B16A16_UNORM",
        122 => "B10G11R11_UFLOAT_PACK32", 124 => "D16_UNORM", 125 => "X8_D24_UNORM_PACK32", 126 => "D32_SFLOAT",
        129 => "D24_UNORM_S8_UINT", 130 => "D32_SFLOAT_S8_UINT", 100 => "R32G32B32A32_SFLOAT",
        _ => "?",
    }
}

/// The 8-bit UNORM colour formats with an sRGB twin of the same layout.
fn srgb_twin(f: i64) -> Option<i64> {
    match f {
        37 => Some(43),
        44 => Some(50),
        _ => None,
    }
}

/// The runtime's own `xrEnumerateSwapchainFormats` for `session`, in its
/// order, which the engine never asks for.
fn runtime_formats(session: u64) -> Option<Vec<i64>> {
    let f = host_export("xrEnumerateSwapchainFormats")?;
    let call = |cap: u32, n: *mut u32, out: *mut i64| {
        // SAFETY: the loader's export, (XrSession, uint32_t, uint32_t*, int64_t*).
        unsafe {
            cordial_guest::invoke("xrEnumerateSwapchainFormats", f as *const c_void, &[Ty::U64, Ty::U32, Ty::Ptr, Ty::Ptr],
                                  &[session, cap as u64, n as u64, out as u64])
        }.ok().map(|o| o.rax as i32)
    };
    let mut n = 0u32;
    if call(0, &mut n, std::ptr::null_mut())? != XR_SUCCESS {
        return None;
    }
    let mut v = vec![0i64; n as usize];
    if call(n, &mut n, v.as_mut_ptr())? != XR_SUCCESS {
        return None;
    }
    v.truncate(n as usize);
    Some(v)
}

// XrHapticActionInfo { type, next, XrAction action, XrPath subactionPath } and
// XrHapticVibration { type, next, XrDuration duration, float frequency, float
// amplitude }: 32 bytes each on both architectures by the layout gate, which
// `tests::haptic_structs_are_the_sizes_these_offsets_assume` holds this to.
const HAPTIC_ACTION: usize = 16;
const HAPTIC_SUBACTION: usize = 24;
const HAPTIC_DURATION: usize = 16;
const HAPTIC_FREQUENCY: usize = 24;
const HAPTIC_AMPLITUDE: usize = 28;
const XR_TYPE_HAPTIC_VIBRATION: u32 = 13;

/// The last vibration forwarded per (action, subaction path): its duration,
/// frequency and amplitude, and when.
static HAPTICS: std::sync::LazyLock<Mutex<HashMap<(u64, u64), (i64, u32, u32, Instant)>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
static HAPTIC_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static HAPTIC_DROPPED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// `xrApplyHapticFeedback(session, const XrHapticActionInfo*, const
/// XrHapticBaseHeader*)`, with a repeat of the vibration still playing
/// answered here.
///
/// Through WiVRn a hover tick buzzed constantly, changed about once a second
/// and went on for about two seconds after the hover ended. WiVRn turns every
/// call into a packet to the headset and a restart of the vibration there,
/// so an engine re-applying the same vibration every frame -- cheap on the
/// Quest's own runtime -- reads as a backlog: that is the INFERRED cause, and
/// the calls are logged so a run in the headset can confirm or refute it. A
/// call identical to the last one for the same action and path is answered
/// with success, which is what the runtime would have said, while less than
/// half its duration has passed; after that it is forwarded, so a vibration
/// the engine sustains on purpose stays continuous. A non-positive duration
/// is taken as Monado's minimum pulse, 100 ms. `CORDIAL_XR_HAPTIC_COALESCE=0`
/// forwards every call, as the control.
///
/// That rule never fires on the engine's UI tick, which is 1000 ns long, so
/// half its duration has always passed. `CORDIAL_XR_HAPTIC_HOLD_MS=<n>`
/// widens the window to `n` ms for identical repeats. It is a diagnostic,
/// off by default: with it, a stream of repeats reaches the runtime at most
/// once per `n` ms, so a headset run can tell whether the buzz follows the
/// number of calls forwarded (a transport that turns each into a restart) or
/// something else.
fn apply_haptic(host: usize) -> Handler {
    let coalesce = std::env::var_os("CORDIAL_XR_HAPTIC_COALESCE").is_none_or(|v| v != "0");
    let hold = std::env::var("CORDIAL_XR_HAPTIC_HOLD_MS").ok().and_then(|v| v.parse::<u64>().ok())
        .map_or(std::time::Duration::ZERO, std::time::Duration::from_millis);
    if !hold.is_zero() {
        say(format!("CORDIAL_XR_HAPTIC_HOLD_MS: identical haptic repeats within {hold:?} are answered here"));
    }
    Box::new(move |c| {
        let (session, info, feedback) = (c.x(0), c.x(1), c.x(2));
        let n = HAPTIC_CALLS.fetch_add(1, Ordering::Relaxed) + 1;
        let mut skip = false;
        let mut seen = None;
        if info != 0 && feedback != 0 {
            // SAFETY: the guest's XrHapticActionInfo and XrHapticBaseHeader.
            let (action, path, ty) = unsafe {
                (*((info as usize + HAPTIC_ACTION) as *const u64),
                 *((info as usize + HAPTIC_SUBACTION) as *const u64),
                 *(feedback as *const u32))
            };
            if ty == XR_TYPE_HAPTIC_VIBRATION {
                // SAFETY: an XrHapticVibration, by its type.
                let (duration, frequency, amplitude) = unsafe {
                    (*((feedback as usize + HAPTIC_DURATION) as *const i64),
                     *((feedback as usize + HAPTIC_FREQUENCY) as *const u32),
                     *((feedback as usize + HAPTIC_AMPLITUDE) as *const u32))
                };
                if n <= 20 {
                    say(format!(
                        "xrApplyHapticFeedback #{n}: action {action:#x} path {path:#x}, duration {duration} ns, \
                         frequency {} Hz, amplitude {}",
                        f32::from_bits(frequency), f32::from_bits(amplitude)
                    ));
                }
                let now = Instant::now();
                let mut g = HAPTICS.lock().unwrap_or_else(|e| e.into_inner());
                if coalesce {
                    if let Some(&(d, f, a, at)) = g.get(&(action, path)) {
                        let lasts = std::time::Duration::from_nanos(if d > 0 { d as u64 } else { 100_000_000 });
                        skip = d == duration && f == frequency && a == amplitude && now.duration_since(at) < (lasts / 2).max(hold);
                    }
                }
                if !skip {
                    g.insert((action, path), (duration, frequency, amplitude, now));
                }
                seen = Some((action, path, duration, f32::from_bits(frequency), f32::from_bits(amplitude)));
            }
        }
        if let Some((action, path, d, f, a)) = seen.filter(|_| input::enabled()) {
            input::haptic(Some((d, f, a)), action, path, !skip);
        }
        if n.is_power_of_two() && n >= 64 {
            say(format!("xrApplyHapticFeedback: {n} calls, {} answered here as repeats of a vibration still playing",
                        HAPTIC_DROPPED.load(Ordering::Relaxed)));
        }
        if skip {
            HAPTIC_DROPPED.fetch_add(1, Ordering::Relaxed);
            c.set_x(0, XR_SUCCESS as i64 as u64);
            return Ok(());
        }
        // SAFETY: the host function, called with the guest's own arguments.
        let r = unsafe {
            cordial_guest::invoke("xrApplyHapticFeedback", host as *const c_void, &[Ty::U64, Ty::Ptr, Ty::Ptr], &[session, info, feedback])
        }?.rax as i32;
        c.set_x(0, r as i64 as u64);
        Ok(())
    })
}

/// `xrStopHapticFeedback(session, const XrHapticActionInfo*)`: always
/// forwarded, and forgets the vibration so the next one is never taken for a
/// repeat.
fn stop_haptic(host: usize) -> Handler {
    Box::new(move |c| {
        let (session, info) = (c.x(0), c.x(1));
        if info != 0 {
            // SAFETY: the guest's XrHapticActionInfo.
            let key = unsafe {
                (*((info as usize + HAPTIC_ACTION) as *const u64), *((info as usize + HAPTIC_SUBACTION) as *const u64))
            };
            HAPTICS.lock().unwrap_or_else(|e| e.into_inner()).remove(&key);
            if input::enabled() {
                input::haptic(None, key.0, key.1, true);
            }
        }
        // SAFETY: the host function, called with the guest's own arguments.
        let r = unsafe {
            cordial_guest::invoke("xrStopHapticFeedback", host as *const c_void, &[Ty::U64, Ty::Ptr], &[session, info])
        }?.rax as i32;
        c.set_x(0, r as i64 as u64);
        Ok(())
    })
}

/// `xrCreateSwapchain(session, const XrSwapchainCreateInfo*, XrSwapchain*)`.
fn create_swapchain(host: usize) -> Handler {
    let as_asked = std::env::var_os("CORDIAL_XR_SWAPCHAIN_AS_ASKED").is_some();
    Box::new(move |c| {
        let (session, info, out) = (c.x(0), c.x(1), c.x(2));
        let formats = runtime_formats(session);
        if let Some(fs) = &formats {
            let named: Vec<String> = fs.iter().map(|&f| format!("{f} {}", vk_format_name(f))).collect();
            say_once(format!("xrEnumerateSwapchainFormats, asked by the bridge (the engine never asks): {} formats, \
                              in the runtime's order: [{}]", fs.len(), named.join(", ")));
        }
        let fo = off("XrSwapchainCreateInfo", "format");
        let uo = off("XrSwapchainCreateInfo", "usageFlags");
        // SAFETY: the guest's XrSwapchainCreateInfo.
        let (asked, usage) = unsafe { (*((info as usize + fo) as *const i64), *((info as usize + uo) as *const u64)) };
        let twin = srgb_twin(asked).filter(|t| formats.as_ref().is_some_and(|fs| fs.contains(t)));
        let twin = twin.filter(|_| !as_asked);
        // The window mirror blits out of the left eye, which needs
        // TRANSFER_SRC; the engine asks for COLOR_ATTACHMENT | SAMPLED, and
        // Monado grants exactly the usage asked. Added in the same host copy
        // as the sRGB twin, never in the guest's struct.
        let transfer_src = crate::android::xr_mirror::XR_USAGE_TRANSFER_SRC;
        let widen = crate::android::xr_mirror::enabled() && usage & XR_SWAPCHAIN_USAGE_COLOR_ATTACHMENT_BIT != 0;
        let mut new_usage = usage;
        if twin.is_some() {
            new_usage |= XR_SWAPCHAIN_USAGE_MUTABLE_FORMAT_BIT;
        }
        if widen {
            new_usage |= transfer_src;
        }
        let mut copy: Vec<u8>;
        let mut pass = info;
        let made = twin.unwrap_or(asked);
        if made != asked || new_usage != usage {
            let size = type_size(XR_TYPE_SWAPCHAIN_CREATE_INFO).map_or(64, |t| t.1);
            // SAFETY: `size` bytes of the guest's create info; the copy keeps
            // its `next` pointer, which is the guest's own chain.
            copy = unsafe { std::slice::from_raw_parts(info as *const u8, size) }.to_vec();
            copy[fo..fo + 8].copy_from_slice(&made.to_ne_bytes());
            copy[uo..uo + 8].copy_from_slice(&new_usage.to_ne_bytes());
            pass = copy.as_ptr() as u64;
            if new_usage & transfer_src != usage & transfer_src {
                say_once(format!("xrCreateSwapchain: usage {usage:#x} -> {new_usage:#x} in a host copy, so the \
                                  window mirror may read it"));
            }
        }
        // SAFETY: the host function, the guest's session and out-pointer, and
        // the guest's create info or a copy that outlives the call.
        let r = unsafe {
            cordial_guest::invoke("xrCreateSwapchain", host as *const c_void, &[Ty::U64, Ty::Ptr, Ty::Ptr], &[session, pass, out])
        }?.rax as i32;
        if made != asked {
            say(format!("xrCreateSwapchain: the engine asked for VkFormat {asked} ({}); made as {made} ({}) with \
                         MUTABLE_FORMAT, since its bytes are sRGB-encoded -> {}",
                        vk_format_name(asked), vk_format_name(made), result_name(r)));
        } else if as_asked && twin.is_some() {
            say_once(format!("xrCreateSwapchain: CORDIAL_XR_SWAPCHAIN_AS_ASKED is set; VkFormat {asked} ({}) passed through",
                             vk_format_name(asked)));
        }
        say_once(format!("xrCreateSwapchain -> {}", result_name(r)));
        if r == XR_SUCCESS {
            // SAFETY: the guest's create info and the handle the runtime wrote.
            // SAFETY: the handle the runtime just wrote.
            let handle = unsafe { *(out as *const u64) };
            // SAFETY: the guest's create info.
            unsafe { note_swapchain(info, handle, made) };
            if let Some(s) = XR.lock().unwrap().swapchains.get_mut(&handle) {
                s.transfer_src = new_usage & transfer_src != 0;
            }
        }
        c.set_x(0, r as i64 as u64);
        Ok(())
    })
}

/// Whether `image` is one of the runtime's swapchain images, and the
/// format the runtime made it with.
pub(crate) fn swapchain_image_format(image: u64) -> Option<i64> {
    XR.lock().unwrap().swapchains.values().find(|s| s.images.contains(&image)).map(|s| s.made)
}

/// `xrCreateDebugUtilsMessengerEXT(instance, const XrDebugUtilsMessengerCreateInfoEXT*, XrDebugUtilsMessengerEXT*)`.
fn create_messenger(host: usize) -> Handler {
    Box::new(move |c| {
        let mut keep = Vec::new();
        // SAFETY: the guest's create info.
        let (copy, _) = unsafe { copy_chain(c.runtime(), "xrCreateDebugUtilsMessengerEXT", c.x(1), &[], &mut keep) }?;
        // SAFETY: the host function, the guest's handle and out-pointer, and a host copy.
        let r = unsafe {
            cordial_guest::invoke("xrCreateDebugUtilsMessengerEXT", host as *const c_void, &[Ty::U64, Ty::Ptr, Ty::Ptr],
                                  &[c.x(0), copy, c.x(2)])
        }?.rax as i32;
        say(format!("xrCreateDebugUtilsMessengerEXT -> {}, callback behind a host-to-guest entry", result_name(r)));
        c.set_x(0, r as i64 as u64);
        Ok(())
    })
}

/// `xrCreateVulkanInstanceKHR`/`xrCreateVulkanDeviceKHR(instance, const
/// Xr*CreateInfoKHR*, Vk* out, VkResult* out)`: the runtime calls
/// `pfnGetInstanceProcAddr` and creates the Vulkan object from the nested
/// create info, adding what it needs. The guest's getter becomes Cordial's
/// native one and the nested chain and allocator are translated as
/// `guest_vk` translates them.
fn create_vulkan(cmd: &'static str, host: usize, sname: &'static str) -> Handler {
    Box::new(move |c| {
        let rt = c.runtime();
        let info = c.x(1);
        if info == 0 {
            c.set_x(0, XR_ERROR_VALIDATION_FAILURE as i64 as u64);
            return Ok(());
        }
        let mut keep = Vec::new();
        // SAFETY: the guest's create info.
        let (copy, _) = unsafe { copy_chain(rt, cmd, info, &[], &mut keep) }?;
        let hc = copy as *mut u8;
        let pfn_off = off(sname, "pfnGetInstanceProcAddr");
        // SAFETY: the host copy.
        let guest_gipa = unsafe { *(hc.add(pfn_off) as *const u64) };
        let host_gipa = crate::guest_vk::host_gipa_for(rt, guest_gipa).ok_or_else(|| unsupported(cmd, format!(
            "pfnGetInstanceProcAddr {guest_gipa:#x} is not the guest's own vkGetInstanceProcAddr stub, and an \
             arm64 function cannot be handed to the runtime")))?;
        let ci_off = off(sname, "vulkanCreateInfo");
        let al_off = off(sname, "vulkanAllocator");
        let mut vk_keep = Vec::new();
        // SAFETY: the host copy's fields, which still hold the guest's pointers.
        let (ci, al) = unsafe { (*(hc.add(ci_off) as *const u64), *(hc.add(al_off) as *const u64)) };
        // SAFETY: the guest's VkInstanceCreateInfo or VkDeviceCreateInfo.
        let host_ci = unsafe { crate::guest_vk::translate_chain(rt, cmd, ci, &mut vk_keep) }?;
        let host_al = crate::guest_vk::allocator(rt, al)?;
        // SAFETY: the host copy.
        unsafe {
            *(hc.add(pfn_off) as *mut u64) = host_gipa as u64;
            *(hc.add(ci_off) as *mut u64) = host_ci;
            *(hc.add(al_off) as *mut u64) = host_al;
        }
        // VkInstanceCreateInfo and VkDeviceCreateInfo both have
        // enabledExtensionCount at 48 and ppEnabledExtensionNames at 56.
        let exts = if ci == 0 { Vec::new() } else {
            // SAFETY: the guest's Vulkan create info, read only.
            unsafe { strings(*((ci + 56) as *const u64), *((ci + 48) as *const u32) as usize) }
        };
        let phys = if sname == "XrVulkanDeviceCreateInfoKHR" {
            // SAFETY: vulkanPhysicalDevice, a handle.
            unsafe { *(hc.add(off(sname, "vulkanPhysicalDevice")) as *const u64) }
        } else { 0 };
        // SAFETY: the host function, the guest's handle and out-pointers, and
        // host copies alive for the call.
        let r = unsafe {
            cordial_guest::invoke(cmd, host as *const c_void, &[Ty::U64, Ty::Ptr, Ty::Ptr, Ty::Ptr],
                                  &[c.x(0), copy, c.x(2), c.x(3)])
        }?.rax as i32;
        // SAFETY: the guest's out-pointers, written by the runtime.
        let (handle, vkr) = unsafe {
            (if c.x(2) != 0 { *(c.x(2) as *const u64) } else { 0 }, if c.x(3) != 0 { *(c.x(3) as *const i32) } else { 0 })
        };
        let mut line = format!("{cmd}: the engine's {} Vulkan extensions [{}], pfnGetInstanceProcAddr {guest_gipa:#x} \
                                (the guest stub) -> Cordial's native one -> {} (VkResult {vkr}, handle {handle:#x})",
                               exts.len(), exts.iter().map(|(_, s)| s.as_str()).collect::<Vec<_>>().join(" "),
                               result_name(r));
        if sname == "XrVulkanDeviceCreateInfoKHR" && r == XR_SUCCESS {
            let (seen_phys, seen_dev) = crate::android::vulkan::created_device();
            line += &format!("; physical device {phys:#x}; Cordial's vkCreateDevice saw physical {seen_phys:#x}, \
                              device {seen_dev:#x}: {}",
                             if seen_dev == handle && seen_phys == phys { "the same host handles" } else { "DIFFERENT" });
        }
        say(line);
        drop(keep);
        drop(vk_keep);
        c.set_x(0, r as i64 as u64);
        Ok(())
    })
}

// ----------------------------------------------------- frames and capture

struct Swapchain {
    /// What the engine asked for.
    format: i64,
    /// What the runtime was asked for (`create_swapchain`).
    made: i64,
    width: u32,
    height: u32,
    array: u32,
    samples: u32,
    images: Vec<u64>,
    acquired: VecDeque<u32>,
    /// Whether the images may be read by a transfer, which the mirror's
    /// blit needs (`widen_swapchain_usage`).
    transfer_src: bool,
}

/// x, y, width, height.
type Rect = (i32, i32, u32, u32);

#[derive(Default)]
struct XrState {
    /// instance, physical device, device, queue family, queue index.
    binding: Option<(u64, u64, u64, u32, u32)>,
    swapchains: HashMap<u64, Swapchain>,
    /// The left eye of the last projection layer submitted while a capture
    /// was pending: swapchain, array index, rect.
    target: Option<(u64, u32, Rect)>,
    /// Both eyes of the last projection layer submitted, for the window
    /// mirror (`xr_mirror`): swapchain, array index, rect and field of view
    /// (angleLeft, angleRight, angleUp, angleDown), left first.
    mirror: Option<[Option<(u64, u32, Rect, [f32; 4])>; 2]>,
}

static XR: std::sync::LazyLock<Mutex<XrState>> = std::sync::LazyLock::new(|| Mutex::new(XrState::default()));

pub static END_FRAMES: AtomicU64 = AtomicU64::new(0);
static LAST_PERIOD: AtomicU64 = AtomicU64::new(0);
static SESSION_STATE: AtomicI32 = AtomicI32::new(-1);

fn state_name(s: i32) -> &'static str {
    match s {
        0 => "UNKNOWN", 1 => "IDLE", 2 => "READY", 3 => "SYNCHRONIZED", 4 => "VISIBLE", 5 => "FOCUSED",
        6 => "STOPPING", 7 => "LOSS_PENDING", 8 => "EXITING", -1 => "none", _ => "?",
    }
}

/// For `cordial_info`: XR frames are `xrEndFrame` calls, not presents.
pub fn info() -> Option<String> {
    let n = END_FRAMES.load(Ordering::Relaxed);
    let s = SESSION_STATE.load(Ordering::Relaxed);
    if n == 0 && s < 0 {
        return None;
    }
    Some(format!("xr_frames={n} (xrEndFrame calls; an XR engine never presents) xr_session={} \
                  xr_predicted_period_ns={} {}", state_name(s), LAST_PERIOD.load(Ordering::Relaxed),
                 crate::android::xr_mirror::info()))
}

/// # Safety
///
/// `info` is the guest's XrSessionCreateInfo; `out` its XrSession*.
unsafe fn note_session(info: u64, out: u64, r: i32) {
    // SAFETY: as the caller promises; next is at 8.
    let mut p = if info == 0 { 0 } else { unsafe { *((info + 8) as *const u64) } };
    while p != 0 {
        // SAFETY: a link of the guest's chain.
        if unsafe { *(p as *const u32) } == XR_TYPE_GRAPHICS_BINDING_VULKAN_KHR {
            let g = |m: &str| p as usize + off("XrGraphicsBindingVulkanKHR", m);
            // SAFETY: an XrGraphicsBindingVulkanKHR.
            let b = unsafe {
                (*(g("instance") as *const u64), *(g("physicalDevice") as *const u64), *(g("device") as *const u64),
                 *(g("queueFamilyIndex") as *const u32), *(g("queueIndex") as *const u32))
            };
            // SAFETY: the handle the runtime just wrote.
            let session = if r == XR_SUCCESS && out != 0 { unsafe { *(out as *const u64) } } else { 0 };
            say(format!("xrCreateSession -> {} (session {session:#x}), Vulkan binding: instance {:#x}, physical \
                         device {:#x}, device {:#x}, queue family {} index {}", result_name(r), b.0, b.1, b.2, b.3, b.4));
            if r == XR_SUCCESS {
                XR.lock().unwrap().binding = Some(b);
                crate::android::xr_mirror::SESSION.store(true, Ordering::Relaxed);
            }
            return;
        }
        // SAFETY: a link of the guest's chain.
        p = unsafe { *((p + 8) as *const u64) };
    }
    say(format!("xrCreateSession -> {} with no Vulkan graphics binding", result_name(r)));
}

/// # Safety
///
/// `info` is the guest's XrSwapchainCreateInfo.
unsafe fn note_swapchain(info: u64, handle: u64, made: i64) {
    let g = |m: &str| info as usize + off("XrSwapchainCreateInfo", m);
    // SAFETY: as the caller promises.
    let s = unsafe {
        Swapchain {
            format: *(g("format") as *const i64),
            made,
            width: *(g("width") as *const u32),
            height: *(g("height") as *const u32),
            array: *(g("arraySize") as *const u32),
            samples: *(g("sampleCount") as *const u32),
            images: Vec::new(),
            acquired: VecDeque::new(),
            transfer_src: false,
        }
    };
    // SAFETY: as the caller promises.
    let usage = unsafe { *(g("usageFlags") as *const u64) };
    let mut s = s;
    s.transfer_src = usage & crate::android::xr_mirror::XR_USAGE_TRANSFER_SRC != 0;
    say(format!("xrCreateSwapchain {handle:#x}: {}x{}, {} layers, VkFormat {}, {} samples, usage {usage:#x}",
                s.width, s.height, s.array, s.format, s.samples));
    XR.lock().unwrap().swapchains.insert(handle, s);
}

/// # Safety
///
/// `arr` holds `n` XrSwapchainImageVulkanKHR the runtime filled.
unsafe fn note_images(swapchain: u64, n: u32, arr: u64) {
    let stride = type_size(1_000_025_001).map_or(24, |t| t.1);
    let at = off("XrSwapchainImageVulkanKHR", "image");
    // SAFETY: as the caller promises.
    let images: Vec<u64> = (0..n as usize).map(|i| unsafe { *((arr as usize + i * stride + at) as *const u64) }).collect();
    if let Some(s) = XR.lock().unwrap().swapchains.get_mut(&swapchain) {
        say_once(format!("xrEnumerateSwapchainImages {swapchain:#x}: {n} VkImages, first {:#x}", images.first().copied().unwrap_or(0)));
        s.images = images;
    }
}

/// The frame log, `CORDIAL_XR_FRAME_LOG=<path>`: one line per `xrEndFrame`,
/// monotonic nanoseconds and the last `predictedDisplayPeriod`.
fn frame_log(now_ns: u64) {
    use std::io::Write;
    static LOG: OnceLock<Option<Mutex<std::io::BufWriter<std::fs::File>>>> = OnceLock::new();
    let log = LOG.get_or_init(|| {
        let p = std::env::var_os("CORDIAL_XR_FRAME_LOG")?;
        std::fs::File::create(&p).ok().map(|f| Mutex::new(std::io::BufWriter::new(f)))
    });
    if let Some(l) = log {
        let mut w = l.lock().unwrap();
        let _ = writeln!(w, "{now_ns} {}", LAST_PERIOD.load(Ordering::Relaxed));
        if END_FRAMES.load(Ordering::Relaxed).is_multiple_of(32) {
            let _ = w.flush();
        }
    }
}

fn monotonic_ns() -> u64 {
    static T0: OnceLock<Instant> = OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_nanos() as u64
}

/// Before `xrEndFrame`: count and time it, say the first frame's layers, and
/// note which image is the left eye, for a pending capture and for the window
/// mirror. The eye is read at the *next* release of that swapchain, since the
/// engine releases its images before it ends the frame.
///
/// # Safety
///
/// `info` is the guest's XrFrameEndInfo.
unsafe fn end_frame(info: u64) {
    let n = END_FRAMES.fetch_add(1, Ordering::Relaxed) + 1;
    frame_log(monotonic_ns());
    if info == 0 {
        return;
    }
    let pending = crate::android::capture::pending();
    let mirror = crate::android::xr_mirror::enabled();
    let say_it = n.is_power_of_two() && n <= 4096;
    if n > 1 && !pending && !mirror && !say_it {
        return;
    }
    let mut left = None;
    let mut eyes: [Option<(u64, u32, Rect, [f32; 4])>; 2] = [None, None];
    let f = |m: &str| info as usize + off("XrFrameEndInfo", m);
    // SAFETY: as the caller promises, and the layer array it points at.
    let (count, layers) = unsafe { (*(f("layerCount") as *const u32), *(f("layers") as *const u64)) };
    let mut desc = Vec::new();
    for i in 0..count as usize {
        // SAFETY: `count` layer pointers.
        let l = unsafe { *((layers as usize + i * 8) as *const u64) };
        if l == 0 {
            continue;
        }
        // SAFETY: a non-null layer pointer from the guest's array.
        let ty = unsafe { *(l as *const u32) };
        if ty != XR_TYPE_COMPOSITION_LAYER_PROJECTION {
            desc.push(format!("type {ty}"));
            continue;
        }
        let pl = |m: &str| l as usize + off("XrCompositionLayerProjection", m);
        // SAFETY: the projection layer the guest submitted.
        let (vc, views) = unsafe { (*(pl("viewCount") as *const u32), *(pl("views") as *const u64)) };
        let stride = type_size(48).map_or(96, |t| t.1);
        let mut vs = Vec::new();
        for v in 0..vc as usize {
            let sub = views as usize + v * stride + off("XrCompositionLayerProjectionView", "subImage");
            // SAFETY: a projection view's XrSwapchainSubImage.
            let (sc, rect, idx) = unsafe {
                let r = sub + off("XrSwapchainSubImage", "imageRect");
                (*(sub as *const u64),
                 (*(r as *const i32), *((r + 4) as *const i32), *((r + 8) as *const u32), *((r + 12) as *const u32)),
                 *((sub + off("XrSwapchainSubImage", "imageArrayIndex")) as *const u32))
            };
            if v == 0 {
                left = Some((sc, idx, rect));
            }
            if v < 2 {
                let fo = views as usize + v * stride + off("XrCompositionLayerProjectionView", "fov");
                // SAFETY: the view's XrFovf: four floats.
                let fov = unsafe { std::ptr::read_unaligned(fo as *const [f32; 4]) };
                eyes[v] = Some((sc, idx, rect, fov));
            }
            if say_it {
                vs.push(format!("swapchain {sc:#x} layer {idx} rect {},{} {}x{}", rect.0, rect.1, rect.2, rect.3));
            }
        }
        if say_it {
            desc.push(format!("projection, {vc} views: {}", vs.join("; ")));
        }
    }
    if left.is_some() && (pending || mirror) {
        let mut st = XR.lock().unwrap();
        if pending {
            st.target = left;
        }
        if mirror {
            st.mirror = Some(eyes);
        }
    }
    if say_it {
        // SAFETY: gettid has no preconditions.
        let tid = unsafe { libc_gettid() };
        say(format!("xrEndFrame #{n} on thread {tid}: {count} layers [{}]", desc.join(" | ")));
    }
}

/// Before `xrReleaseSwapchainImage`: the image the engine is handing back is
/// complete and in `COLOR_ATTACHMENT_OPTIMAL` on its queue, which is the XR
/// equivalent of the moment a present is captured at. If it is the left eye
/// a pending capture asked for, read it now; if it is the left eye at all,
/// mirror it into the window (`xr_mirror`, whose module doc says why the
/// session's queue is safe to submit on here).
fn release(swapchain: u64) {
    let mut st = XR.lock().unwrap();
    let binding = st.binding;
    let target = st.target;
    let mirror = st.mirror;
    let Some(s) = st.swapchains.get_mut(&swapchain) else { return };
    let Some(index) = s.acquired.pop_front() else { return };
    let (image, samples, format, transfer_src) = (s.images.get(index as usize).copied(), s.samples, s.format, s.transfer_src);

    if crate::android::capture::pending() {
        if let Some((_, layer, rect)) = target.filter(|t| t.0 == swapchain) {
            let Some((inst, phys, dev, family, qindex)) = binding else {
                crate::android::capture::abandon("no Vulkan session binding was recorded");
                return;
            };
            let Some(image) = image else {
                crate::android::capture::abandon("the released image index is outside the enumerated images");
                return;
            };
            if samples > 1 {
                crate::android::capture::abandon("the left-eye swapchain is multisampled");
                return;
            }
            let t = crate::android::capture::Target {
                device: dev,
                queue: 0,
                queue_family: family,
                image,
                layout: crate::android::capture::LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
                src_access: crate::android::capture::ACCESS_COLOR_ATTACHMENT_WRITE,
                layer,
                rect,
                format: format as u32,
            };
            st.target = None;
            crate::android::vulkan::capture_image(inst, phys, qindex, t);
        }
    }

    let Some([Some((_, layer, rect, fov)), _]) = mirror.filter(|m| m[0].is_some_and(|l| l.0 == swapchain)) else {
        return;
    };
    let (Some((_, phys, _, family, qindex)), Some(image)) = (binding, image) else { return };
    if samples > 1 || !transfer_src {
        say_once(format!("the window mirror skips swapchain {swapchain:#x}: {}",
                         if samples > 1 { "multisampled" } else { "its usage has no TRANSFER_SRC" }));
        return;
    }
    // Held across the mirror, so an `xrDestroySwapchain` on another thread
    // cannot free the image mid-blit.
    crate::android::xr_mirror::frame(&crate::android::xr_mirror::Eye {
        physical_device: phys,
        queue_family: family,
        queue_index: qindex,
        image,
        format: format as u32,
        layer,
        rect,
        fov,
    });
    drop(st);
}

/// # Safety
///
/// `ev` is the XrEventDataBuffer the runtime just filled.
unsafe fn note_event(ev: u64) {
    // SAFETY: as the caller promises.
    let ty = unsafe { *(ev as *const u32) };
    if ty == XR_TYPE_EVENT_DATA_SESSION_STATE_CHANGED {
        // SAFETY: the event the runtime just wrote, of the type checked above.
        let s = unsafe { *((ev as usize + off("XrEventDataSessionStateChanged", "state")) as *const i32) };
        SESSION_STATE.store(s, Ordering::Relaxed);
        say(format!("session state -> XR_SESSION_STATE_{} (after {} XR frames)", state_name(s),
                    END_FRAMES.load(Ordering::Relaxed)));
    } else {
        say_once(format!("xrPollEvent: event type {ty}"));
    }
}

// ------------------------------------------------------------------ saying

fn say(s: String) {
    eprintln!("[guest] openxr: {s}");
}

fn say_once(s: String) {
    static SAID: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
    if SAID.lock().unwrap().insert(s.clone()) {
        say(s);
    }
}

fn result_name(r: i32) -> String {
    let n = match r {
        0 => "XR_SUCCESS", 1 => "XR_TIMEOUT_EXPIRED", 3 => "XR_SESSION_LOSS_PENDING", 4 => "XR_EVENT_UNAVAILABLE",
        7 => "XR_SPACE_BOUNDS_UNAVAILABLE", 8 => "XR_SESSION_NOT_FOCUSED", 9 => "XR_FRAME_DISCARDED",
        -1 => "XR_ERROR_VALIDATION_FAILURE", -2 => "XR_ERROR_RUNTIME_FAILURE", -3 => "XR_ERROR_OUT_OF_MEMORY",
        -4 => "XR_ERROR_API_VERSION_UNSUPPORTED", -6 => "XR_ERROR_INITIALIZATION_FAILED",
        -7 => "XR_ERROR_FUNCTION_UNSUPPORTED", -8 => "XR_ERROR_FEATURE_UNSUPPORTED",
        -9 => "XR_ERROR_EXTENSION_NOT_PRESENT", -10 => "XR_ERROR_LIMIT_REACHED", -11 => "XR_ERROR_SIZE_INSUFFICIENT",
        -12 => "XR_ERROR_HANDLE_INVALID", -13 => "XR_ERROR_INSTANCE_LOST", -14 => "XR_ERROR_SESSION_RUNNING",
        -16 => "XR_ERROR_SESSION_NOT_RUNNING", -17 => "XR_ERROR_SESSION_LOST", -18 => "XR_ERROR_SYSTEM_INVALID",
        -19 => "XR_ERROR_PATH_INVALID", -20 => "XR_ERROR_PATH_COUNT_EXCEEDED", -21 => "XR_ERROR_PATH_FORMAT_INVALID",
        -22 => "XR_ERROR_PATH_UNSUPPORTED", -23 => "XR_ERROR_LAYER_INVALID", -24 => "XR_ERROR_LAYER_LIMIT_EXCEEDED",
        -25 => "XR_ERROR_SWAPCHAIN_RECT_INVALID", -26 => "XR_ERROR_SWAPCHAIN_FORMAT_UNSUPPORTED",
        -27 => "XR_ERROR_ACTION_TYPE_MISMATCH", -28 => "XR_ERROR_SESSION_NOT_READY",
        -29 => "XR_ERROR_SESSION_NOT_STOPPING", -30 => "XR_ERROR_TIME_INVALID",
        -31 => "XR_ERROR_REFERENCE_SPACE_UNSUPPORTED", -35 => "XR_ERROR_ACTIONSET_NOT_ATTACHED",
        -36 => "XR_ERROR_ACTIONSETS_ALREADY_ATTACHED", -37 => "XR_ERROR_LOCALIZED_NAME_DUPLICATED",
        -38 => "XR_ERROR_LOCALIZED_NAME_INVALID", -39 => "XR_ERROR_GRAPHICS_REQUIREMENTS_CALL_MISSING",
        -40 => "XR_ERROR_RUNTIME_UNAVAILABLE", -46 => "XR_ERROR_GRAPHICS_DEVICE_INVALID",
        -47 => "XR_ERROR_POSE_INVALID", -48 => "XR_ERROR_INDEX_OUT_OF_RANGE",
        -49 => "XR_ERROR_VIEW_CONFIGURATION_TYPE_UNSUPPORTED", -50 => "XR_ERROR_ENVIRONMENT_BLEND_MODE_UNSUPPORTED",
        -51 => "XR_ERROR_RUNTIME_UNAVAILABLE", -52 => "XR_ERROR_API_LAYER_NOT_PRESENT",
        -53 => "XR_ERROR_CALL_ORDER_INVALID",
        -1_000_101_000 => "XR_ERROR_DISPLAY_REFRESH_RATE_UNSUPPORTED_FB",
        _ => return format!("XrResult {r}"),
    };
    n.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn elf_words(path: &Path, sym: &str) -> Vec<u64> {
        let d = std::fs::read(path).unwrap();
        let u16_at = |o: usize| u16::from_le_bytes(d[o..o + 2].try_into().unwrap()) as usize;
        let u32_at = |o: usize| u32::from_le_bytes(d[o..o + 4].try_into().unwrap()) as usize;
        let u64_at = |o: usize| u64::from_le_bytes(d[o..o + 8].try_into().unwrap()) as usize;
        let wide = d[4] == 2;
        let (shoff, shentsize, shnum) =
            if wide { (u64_at(0x28), u16_at(0x3a), u16_at(0x3c)) } else { (u32_at(0x20), u16_at(0x2e), u16_at(0x30)) };
        let sec = |i: usize| {
            let b = shoff + i * shentsize;
            if wide { (u32_at(b + 4), u64_at(b + 24), u64_at(b + 32), u32_at(b + 40)) }
            else { (u32_at(b + 4), u32_at(b + 16), u32_at(b + 20), u32_at(b + 24)) }
        };
        let symsize = if wide { 24 } else { 16 };
        for i in 0..shnum {
            let (ty, o, size, link) = sec(i);
            if ty != 2 {
                continue;
            }
            let (_, stroff, _, _) = sec(link as usize);
            for k in 0..size / symsize {
                let e = o + k * symsize;
                let (name_off, shndx, value, sz) = if wide {
                    (u32_at(e), u16_at(e + 6), u64_at(e + 8), u64_at(e + 16))
                } else {
                    (u32_at(e), u16_at(e + 14), u32_at(e + 4), u32_at(e + 8))
                };
                let s = stroff + name_off;
                let end = s + d[s..].iter().position(|&b| b == 0).unwrap();
                if &d[s..end] != sym.as_bytes() || shndx == 0 || shndx >= 0xff00 {
                    continue;
                }
                let (_, soff, _, _) = sec(shndx);
                let bytes = &d[soff + value..soff + value + sz];
                // The probe is uint64_t on every target, i686 included.
                return bytes.chunks(8).map(|c| u64::from_le_bytes(c.try_into().unwrap())).collect();
            }
        }
        panic!("{sym} not in {}", path.display());
    }

    fn include_dirs() -> (PathBuf, PathBuf) {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../third_party/openxr/include");
        let vk = PathBuf::from(std::env::var("CORDIAL_VK_INCLUDE").unwrap_or_else(|_| "/usr/include".into()));
        (root, vk)
    }

    fn compile_probe(target: &str, dir: &Path) -> Vec<u64> {
        let (xr, vk) = include_dirs();
        let inc = dir.join(format!("vk-{target}"));
        std::fs::create_dir_all(&inc).unwrap();
        for sub in ["vulkan", "vk_video"] {
            let link = inc.join(sub);
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(vk.join(sub), &link).unwrap();
        }
        let src = dir.join("probe.c");
        std::fs::write(&src, include_str!("guest_xr_probe.c")).unwrap();
        let obj = dir.join(format!("probe-{target}.o"));
        let out = Command::new("clang")
            .args([&format!("--target={target}"), "-ffreestanding", "-nostdlibinc", "-std=c11", "-w", "-c"])
            .arg("-I").arg(&xr).arg("-I").arg(&inc).arg(&src).arg("-o").arg(&obj)
            .output()
            .expect("clang, which building Cordial already requires");
        assert!(out.status.success(), "the probe did not compile for {target}: {}", String::from_utf8_lossy(&out.stderr));
        elf_words(&obj, "cordial_xr_probe")
    }

    fn labels() -> Vec<String> {
        let mut words = Vec::new();
        for line in include_str!("guest_xr_probe.c").lines().map(str::trim) {
            if let Some(r) = line.strip_prefix("S(") {
                let t = r.split(',').next().unwrap();
                words.extend([format!("sizeof({t})"), format!("alignof({t})"), format!("type of {t}")]);
            } else if let Some(r) = line.strip_prefix("M(") {
                words.push(format!("offsetof({}", r.trim_end_matches(')').to_string() + ")"));
            }
        }
        words.push("terminator".into());
        words
    }

    fn differences(a: &[u64], b: &[u64]) -> Vec<String> {
        let l = labels();
        assert_eq!(a.len(), l.len(), "probe words and labels disagree");
        if a.len() != b.len() {
            return vec![format!("probe length {} against {}", a.len(), b.len())];
        }
        l.iter().zip(a.iter().zip(b)).filter(|(_, (x, y))| x != y).map(|(l, (x, y))| format!("{l}: {x} against {y}")).collect()
    }

    /// The OpenXR half of design §3.2's gate: every struct and union
    /// `openxr.h` and `openxr_platform.h` declare with the Android and Vulkan
    /// parts on, as the Quest engine's compiler lays them out and as the
    /// host's does, with 0 differences. The control is i686, which must show
    /// differences, or the diff could be comparing nothing. The generated
    /// sizes and offsets the bridge reads are checked against these headers.
    #[test]
    fn layout_gate() {
        // Skipped, not failed, without a clang that can target all three
        // (`guest_vk::layout_gate_cannot_compile` says why).
        if let Some(why) = crate::guest_vk::layout_gate_cannot_compile() {
            println!("xr layout gate skipped: {why}");
            return;
        }
        let dir = std::env::temp_dir().join(format!("cordial-xr-gate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let arm = compile_probe("aarch64-linux-android26", &dir);
        let x86 = compile_probe("x86_64-linux-gnu", &dir);
        let i686 = compile_probe("i686-linux-gnu", &dir);
        let lw = labels();
        let types = lw.iter().filter(|l| l.starts_with("sizeof(")).count();
        let d = differences(&arm, &x86);
        let control = differences(&arm, &i686);
        println!("xr layout gate: XR_CURRENT_API_VERSION {:#x}, openxr.h + openxr_platform.h (Android, Vulkan) at {}",
                 table::API_VERSION, include_dirs().0.display());
        println!("xr layout gate: {types} structs and unions, {} words", lw.len() - 1);
        println!("xr layout gate: aarch64-linux-android26 against x86_64-linux-gnu: {} differences", d.len());
        for x in &d {
            println!("  {x}");
        }
        println!("xr layout gate: control, aarch64-linux-android26 against i686-linux-gnu: {} differences, e.g. {}",
                 control.len(), control.first().map_or("", String::as_str));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(d.is_empty(), "{} layout differences", d.len());
        assert!(control.len() > 100, "the control found only {} differences", control.len());

        let mut size_of = HashMap::new();
        let mut type_of = HashMap::new();
        let mut offset_of = HashMap::new();
        for (i, l) in lw.iter().enumerate() {
            if let Some(t) = l.strip_prefix("sizeof(").and_then(|r| r.strip_suffix(')')) {
                size_of.insert(t.to_owned(), x86[i] as usize);
                type_of.insert(t.to_owned(), x86[i + 2]);
            } else if let Some(r) = l.strip_prefix("offsetof(").and_then(|r| r.strip_suffix(')')) {
                offset_of.insert(r.to_owned(), x86[i] as usize);
            }
        }
        for &(ty, name, size) in &table::TYPE_SIZES {
            assert_eq!(size_of.get(name), Some(&size), "{name}'s generated size");
            assert_eq!(type_of.get(name), Some(&(ty as u64)), "{name}'s generated type");
        }
        // copy_chain reads type at 0 and next at 8 of any link.
        for (t, &ty) in &type_of {
            if ty != u64::MAX {
                assert_eq!(offset_of.get(&format!("{t}, type")), Some(&0), "{t}.type");
                assert_eq!(offset_of.get(&format!("{t}, next")), Some(&8), "{t}.next");
            }
        }
        for &(s, m, o) in &table::OFFSETS {
            assert_eq!(offset_of.get(&format!("{s}, {m}")), Some(&o), "{s}.{m}'s generated offset");
        }
        for &(s, _, o, m, _) in &table::PFN_MEMBERS {
            assert_eq!(offset_of.get(&format!("{s}, {m}")), Some(&o), "{s}.{m}");
        }
        // Hand-written offsets in create_instance: XrApplicationInfo's
        // engineName and apiVersion.
        assert_eq!(offset_of.get("XrApplicationInfo, engineName"), Some(&132));
        assert_eq!(offset_of.get("XrApplicationInfo, apiVersion"), Some(&264));
        assert_eq!(type_size(2).map(|t| t.0), Some("XrExtensionProperties"));
        assert_eq!(offset_of.get("XrExtensionProperties, extensionName"), Some(&16));
    }

    /// Everything the bridge reads by name is generated, and the callback
    /// and Vulkan-pointer members are exactly the ones handled by hand.
    #[test]
    fn haptic_structs_are_the_sizes_these_offsets_assume() {
        assert_eq!(type_size(XR_TYPE_HAPTIC_VIBRATION), Some(("XrHapticVibration", 32)));
        assert_eq!(type_size(59), Some(("XrHapticActionInfo", 32)));
    }

    #[test]
    fn every_offset_used_is_generated() {
        for (s, m) in [
            ("XrInstanceCreateInfo", "enabledExtensionCount"), ("XrInstanceCreateInfo", "enabledExtensionNames"),
            ("XrInstanceCreateInfo", "applicationInfo"), ("XrSystemProperties", "systemName"),
            ("XrSystemProperties", "vendorId"), ("XrSystemProperties", "graphicsProperties"),
            ("XrFrameState", "predictedDisplayPeriod"), ("XrLoaderInitInfoAndroidKHR", "applicationVM"),
            ("XrLoaderInitInfoAndroidKHR", "applicationContext"), ("XrGraphicsBindingVulkanKHR", "instance"),
            ("XrGraphicsBindingVulkanKHR", "physicalDevice"), ("XrGraphicsBindingVulkanKHR", "device"),
            ("XrGraphicsBindingVulkanKHR", "queueFamilyIndex"), ("XrGraphicsBindingVulkanKHR", "queueIndex"),
            ("XrSwapchainCreateInfo", "format"), ("XrSwapchainCreateInfo", "width"),
            ("XrSwapchainCreateInfo", "height"), ("XrSwapchainCreateInfo", "arraySize"),
            ("XrSwapchainCreateInfo", "sampleCount"), ("XrSwapchainCreateInfo", "usageFlags"),
            ("XrSwapchainImageVulkanKHR", "image"), ("XrFrameEndInfo", "layerCount"), ("XrFrameEndInfo", "layers"),
            ("XrCompositionLayerProjection", "viewCount"), ("XrCompositionLayerProjection", "views"),
            ("XrCompositionLayerProjectionView", "subImage"), ("XrCompositionLayerProjectionView", "fov"),
            ("XrSwapchainSubImage", "imageRect"),
            ("XrSwapchainSubImage", "imageArrayIndex"), ("XrEventDataSessionStateChanged", "state"),
            ("XrVulkanInstanceCreateInfoKHR", "pfnGetInstanceProcAddr"),
            ("XrVulkanInstanceCreateInfoKHR", "vulkanCreateInfo"), ("XrVulkanInstanceCreateInfoKHR", "vulkanAllocator"),
            ("XrVulkanDeviceCreateInfoKHR", "pfnGetInstanceProcAddr"), ("XrVulkanDeviceCreateInfoKHR", "vulkanCreateInfo"),
            ("XrVulkanDeviceCreateInfoKHR", "vulkanAllocator"), ("XrVulkanDeviceCreateInfoKHR", "vulkanPhysicalDevice"),
        ] {
            off(s, m);
        }
        assert_eq!(off("XrRect2Di", "offset"), 0);
        assert_eq!(off("XrRect2Di", "extent"), 8);
        let pfns: BTreeSet<(&str, &str)> = table::PFN_MEMBERS.iter().map(|r| (r.0, r.3)).collect();
        assert_eq!(pfns, BTreeSet::from([
            ("XrDebugUtilsMessengerCreateInfoEXT", "userCallback"),
            ("XrVulkanDeviceCreateInfoKHR", "pfnGetInstanceProcAddr"),
            ("XrVulkanInstanceCreateInfoKHR", "pfnGetInstanceProcAddr"),
        ]), "a new function-pointer member needs handling");
        let vks: BTreeSet<(&str, &str)> = table::VK_MEMBERS.iter().map(|r| (r.0, r.2)).collect();
        assert_eq!(vks, BTreeSet::from([
            ("XrVulkanDeviceCreateInfoKHR", "vulkanAllocator"), ("XrVulkanDeviceCreateInfoKHR", "vulkanCreateInfo"),
            ("XrVulkanInstanceCreateInfoKHR", "vulkanAllocator"), ("XrVulkanInstanceCreateInfoKHR", "vulkanCreateInfo"),
        ]), "a new pointer to a Vulkan struct needs translating");
        assert_eq!(type_size(XR_TYPE_COMPOSITION_LAYER_PROJECTION).unwrap().0, "XrCompositionLayerProjection");
        assert_eq!(type_size(48).unwrap().0, "XrCompositionLayerProjectionView");
        assert_eq!(type_size(1_000_025_001).unwrap().0, "XrSwapchainImageVulkanKHR");
        assert!(table::TYPE_SIZES.windows(2).all(|w| w[0].0 < w[1].0), "type table not sorted and unique");
        // Every command the Quest build names.
        for n in ["xrGetInstanceProcAddr", "xrInitializeLoaderKHR", "xrCreateVulkanInstanceKHR", "xrCreateVulkanDeviceKHR",
                  "xrGetVulkanGraphicsDevice2KHR", "xrGetVulkanGraphicsRequirements2KHR", "xrGetDisplayRefreshRateFB",
                  "xrRequestDisplayRefreshRateFB", "xrEnumeratePerformanceMetricsCounterPathsMETA",
                  "xrSetPerformanceMetricsStateMETA", "xrGetPerformanceMetricsStateMETA",
                  "xrQueryPerformanceMetricsCounterMETA", "xrEndFrame", "xrPollEvent", "xrLocateViews"] {
            assert!(signature(n).is_some(), "{n}");
        }
    }

    /// xrCreateInstance's copy: the Android link and extension are gone from
    /// what the host sees, a debug messenger's callback is an entry that runs
    /// the guest's, and the guest's own structures are not written.
    #[test]
    fn instance_chain_is_copied_without_the_android_link() {
        let rt = Runtime::new(cordial_guest::Options::default());
        // PFN_xrDebugUtilsMessengerCallbackEXT: return messageTypes as XrBool32. mov w0, w1; ret
        let words: Vec<u8> = [0x2a01_03e0u32, 0xd65f_03c0].iter().flat_map(|w| w.to_le_bytes()).collect();
        let cb = cordial_guest::Mapping::with_contents(&words, 0);
        let tail = [1_000_019_002u64, 0, 0, 0, cb.addr(), 0x77]; // a second messenger, at the end
        let android = [XR_TYPE_INSTANCE_CREATE_INFO_ANDROID_KHR as u64, tail.as_ptr() as u64, 0x1111, 0x2222];
        let mut messenger = [0u64; 6];
        messenger[0] = 1_000_019_002;
        messenger[1] = android.as_ptr() as u64;
        messenger[4] = cb.addr();
        let mut info = vec![0u64; 41];
        info[0] = 3; // XR_TYPE_INSTANCE_CREATE_INFO
        info[1] = messenger.as_ptr() as u64;
        let before = (info.clone(), messenger, android, tail);
        let mut keep = Vec::new();
        // SAFETY: a well-formed chain of ours.
        let (h, stripped) = unsafe {
            copy_chain(&rt, "xrCreateInstance", info.as_ptr() as u64, &[XR_TYPE_INSTANCE_CREATE_INFO_ANDROID_KHR], &mut keep)
        }.unwrap();
        assert_eq!(stripped, vec![XR_TYPE_INSTANCE_CREATE_INFO_ANDROID_KHR]);
        // SAFETY: the copies in `keep`.
        unsafe {
            let hi = h as *const u64;
            assert_ne!(hi as u64, info.as_ptr() as u64);
            let hm = hi.add(1).read() as *const u64;
            assert_eq!(hm.read() as u32, 1_000_019_002);
            let ht = hm.add(1).read() as *const u64;
            assert_eq!(ht.read() as u32, 1_000_019_002, "the Android link is skipped");
            assert_eq!(ht.add(1).read(), 0);
            assert_eq!(ht.add(5).read(), 0x77);
            for p in [hm, ht] {
                let f: extern "C" fn(u64, u64, *const c_void, *mut c_void) -> u32 = std::mem::transmute(p.add(4).read() as usize);
                assert_eq!(f(1, 0x4, std::ptr::null(), std::ptr::null_mut()), 0x4);
            }
        }
        assert_eq!((info, messenger, android, tail), before, "the guest's chain was written");
    }
}
