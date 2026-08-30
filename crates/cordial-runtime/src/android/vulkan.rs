//! Roblox's Vulkan path, interposed onto the host loader.
//!
//! Two separate problems, both visible in the engine's own log before this file
//! existed:
//!
//! ```text
//! [FLog::SurfaceController] Mode 6 failed: Unable to load Vulkan API
//! ```
//!
//! **1. The `dlopen` never reaches the host.** `libroblox.so` does not link
//! Vulkan — it is not in `DT_NEEDED` — it `dlopen`s `"libvulkan.so"` and, failing
//! that, `"libvulkan.so.1"` (see `docs/framework-api-inventory.md` §on Vulkan).
//! Both calls go through the *bionic* linker, which only knows Cordial's virtual
//! library set and has no reason to fall through to the host's `/usr/lib64`.
//! Nothing here loads a real ELF for either name: `get_instance_proc_addr_symbol`
//! is registered as a **virtual library** (see `symtab::build`), the same
//! mechanism `EMPTY_LIBRARIES` uses — bionic's `find_library` matches an
//! already-registered soname before it ever touches disk.
//!
//! **2. Even a working `dlopen` would not be enough.** The virtual library's
//! only export is `vkGetInstanceProcAddr`; every other Vulkan entry point,
//! including `vkCreateInstance` itself, is fetched *through* it — bionic's
//! `"Unable to load Vulkan API: vkCreateInstance is NULL"` message (present as a
//! second, more specific string in the binary) only fires once that dlsym-first
//! step already worked, which confirms this is how the engine bootstraps. So
//! `vk_get_instance_proc_addr` below is the entire interposition surface for
//! global-level Vulkan: every function it does not recognise by name is hostcode,
//! forwarded straight through the real loader.
//!
//! What it does recognise, and why:
//!
//! * `vkCreateAndroidSurfaceKHR` — the engine calls this and only this to get a
//!   surface. Desktop Mesa has never heard of it; on X11 it has
//!   `vkCreateXlibSurfaceKHR` instead, and on Wayland `vkCreateWaylandSurfaceKHR`.
//!   [`vk_create_android_surface_khr`] builds whichever real call from Cordial's
//!   own window — `android::window::current()` on X11,
//!   `android::wayland::current()` on Wayland, decided once by
//!   `android::backend()` — the same handles `egl_create_window_surface`
//!   substitutes for EGL in each backend's own module; see the comment there
//!   for why that translation lives with the window and not in a call-counting
//!   module. This file follows the same reasoning for Vulkan.
//! * `VK_KHR_android_surface` — the extension string that has to exist for the
//!   engine to ask for the function above at all. Mesa reports
//!   `VK_KHR_xlib_surface` or `VK_KHR_wayland_surface` under their own names,
//!   according to which platform is live;
//!   [`vk_enumerate_instance_extension_properties`] adds
//!   `VK_KHR_android_surface` to the host's real list whenever the real
//!   extension for the active backend is present, and [`vk_create_instance`]
//!   rewrites it back before the real `vkCreateInstance` ever sees it — the
//!   host loader must never be told to enable an extension it does not
//!   implement.
//!
//! * `vkCreateSwapchainKHR` — the engine never names a present mode it did not
//!   have to name, and the one it names is `VK_PRESENT_MODE_FIFO_KHR`, which is
//!   a hard vsync lock. [`vk_create_swapchain_khr`] offers
//!   `VK_PRESENT_MODE_MAILBOX_KHR` instead when a setting asks for it. See
//!   that function for the measurement and for why overriding what the engine
//!   asked for is defensible here and would not be for most calls.
//!
//! Everything else — every `vkCmd*`, the whole per-frame surface — is
//! untouched: once a real `VkInstance` exists, forwarding
//! `vkGetInstanceProcAddr(instance, name)` to the host is correct for any name
//! this module does not special-case, because the host's implementation *is* the
//! implementation Cordial wants.

use std::ffi::{c_char, c_ulong, c_void, CStr};
use std::sync::OnceLock;

// ------------------------------------------------------------------ layout
//
// These four structs are laid out exactly as the Vulkan specification defines
// them. Unlike the bionic/glibc boundary elsewhere in this tree, there is no
// second layout to reconcile here — Vulkan's ABI is the same struct on Android
// and on desktop Linux, which is what makes interposing it (rather than
// reimplementing it) the right shape for this problem.

#[repr(C)]
#[derive(Clone, Copy)]
struct VkInstanceCreateInfo {
    s_type: i32,
    p_next: *const c_void,
    flags: u32,
    p_application_info: *const c_void,
    enabled_layer_count: u32,
    pp_enabled_layer_names: *const *const c_char,
    enabled_extension_count: u32,
    pp_enabled_extension_names: *const *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VkExtensionProperties {
    extension_name: [c_char; 256],
    spec_version: u32,
}

impl VkExtensionProperties {
    fn zeroed() -> Self {
        // SAFETY: an all-zero `VkExtensionProperties` (empty name, version 0) is
        // a valid bit pattern for every field.
        unsafe { std::mem::zeroed() }
    }

    fn named(name: &str, spec_version: u32) -> Self {
        let mut p = Self::zeroed();
        for (dst, &b) in p.extension_name.iter_mut().zip(name.as_bytes()) {
            *dst = b as c_char;
        }
        p.spec_version = spec_version;
        p
    }

    fn name_matches(&self, target: &[u8]) -> bool {
        // SAFETY: `extension_name` is always initialised (zeroed or `named`),
        // so reading it as bytes is sound regardless of where the NUL falls.
        let bytes: &[u8] =
            unsafe { std::slice::from_raw_parts(self.extension_name.as_ptr().cast(), 256) };
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(256);
        &bytes[..end] == target
    }
}

#[repr(C)]
struct VkAndroidSurfaceCreateInfoKHR {
    s_type: i32,
    p_next: *const c_void,
    flags: u32,
    window: *mut c_void,
}

#[repr(C)]
struct VkXlibSurfaceCreateInfoKHR {
    s_type: i32,
    p_next: *const c_void,
    flags: u32,
    dpy: *mut c_void,
    window: c_ulong,
}

/// `VK_STRUCTURE_TYPE_XLIB_SURFACE_CREATE_INFO_KHR`. Present in `vulkan_core.h`
/// unguarded (the `sType` enum is platform-independent even though the struct it
/// tags is declared behind `VK_USE_PLATFORM_XLIB_KHR`).
const VK_STRUCTURE_TYPE_XLIB_SURFACE_CREATE_INFO_KHR: i32 = 1000004000;

#[repr(C)]
struct VkWaylandSurfaceCreateInfoKHR {
    s_type: i32,
    p_next: *const c_void,
    flags: u32,
    display: *mut c_void,
    surface: *mut c_void,
}

/// `VK_STRUCTURE_TYPE_WAYLAND_SURFACE_CREATE_INFO_KHR`, same unguarded-`sType`
/// situation as the Xlib one above.
const VK_STRUCTURE_TYPE_WAYLAND_SURFACE_CREATE_INFO_KHR: i32 = 1000006000;

/// `VkExtent2D` and `VkSurfaceCapabilitiesKHR`, read (never constructed) by
/// [`vk_get_physical_device_surface_capabilities_khr`] — see that function for
/// why patching `currentExtent` in this struct is what makes the Wayland
/// backend render at all.
#[repr(C)]
#[derive(Clone, Copy)]
struct VkExtent2D {
    width: u32,
    height: u32,
}

#[repr(C)]
struct VkSurfaceCapabilitiesKHR {
    min_image_count: u32,
    max_image_count: u32,
    current_extent: VkExtent2D,
    min_image_extent: VkExtent2D,
    max_image_extent: VkExtent2D,
    max_image_array_layers: u32,
    supported_transforms: u32,
    current_transform: i32,
    supported_composite_alpha: u32,
    supported_usage_flags: u32,
}

/// `VkSwapchainCreateInfoKHR`, read and — for one field — rewritten by
/// [`vk_create_swapchain_khr`].
///
/// The two 64-bit handles are spelled `u64` rather than `*mut c_void` because
/// that is what they are: `VkSurfaceKHR` and `VkSwapchainKHR` are
/// non-dispatchable, which the specification defines as `uint64_t` on every
/// platform. It makes no difference to the ABI — both occupy eight bytes here
/// and travel in one register — but it does stop the compiler quietly agreeing
/// that a surface handle is a pointer that could be dereferenced.
#[repr(C)]
#[derive(Clone, Copy)]
struct VkSwapchainCreateInfoKHR {
    s_type: i32,
    p_next: *const c_void,
    flags: u32,
    surface: u64,
    min_image_count: u32,
    image_format: i32,
    image_color_space: i32,
    image_extent: VkExtent2D,
    image_array_layers: u32,
    image_usage: u32,
    image_sharing_mode: i32,
    queue_family_index_count: u32,
    p_queue_family_indices: *const u32,
    pre_transform: u32,
    composite_alpha: u32,
    present_mode: i32,
    clipped: u32,
    old_swapchain: u64,
}

/// `VkPresentModeKHR`. Only `FIFO` is guaranteed to exist — the specification
/// requires every implementation to support it and requires nothing of the
/// other three — which is why [`vk_create_swapchain_khr`] asks the driver what
/// it has rather than assuming.
const VK_PRESENT_MODE_IMMEDIATE_KHR: i32 = 0;
const VK_PRESENT_MODE_MAILBOX_KHR: i32 = 1;
const VK_PRESENT_MODE_FIFO_KHR: i32 = 2;
const VK_PRESENT_MODE_FIFO_RELAXED_KHR: i32 = 3;

fn present_mode_name(mode: i32) -> &'static str {
    match mode {
        VK_PRESENT_MODE_IMMEDIATE_KHR => "IMMEDIATE",
        VK_PRESENT_MODE_MAILBOX_KHR => "MAILBOX",
        VK_PRESENT_MODE_FIFO_KHR => "FIFO",
        VK_PRESENT_MODE_FIFO_RELAXED_KHR => "FIFO_RELAXED",
        _ => "unknown",
    }
}

/// `VK_KHR_android_surface`'s spec version, per the Khronos extension registry.
/// Fixed at 6 since it was introduced; there is nothing to detect it against.
const ANDROID_SURFACE_SPEC_VERSION: u32 = 6;

/// The real surface extension `VK_KHR_android_surface` is substituted for,
/// according to whichever display backend [`crate::android::backend`]
/// selected. `backend()` is chosen once, from the environment, before any
/// window opens (see its own doc comment) — Vulkan bring-up always happens
/// after that choice is fixed, so there is no point in this call racing it.
fn real_surface_extension_name() -> &'static CStr {
    match crate::android::backend() {
        crate::android::Backend::Wayland => c"VK_KHR_wayland_surface",
        crate::android::Backend::X11 => c"VK_KHR_xlib_surface",
    }
}

const VK_SUCCESS: i32 = 0;
const VK_INCOMPLETE: i32 = 5;
const VK_ERROR_INITIALIZATION_FAILED: i32 = -3;
const VK_ERROR_EXTENSION_NOT_PRESENT: i32 = -7;
/// The two the WSI returns when the swapchain no longer matches the surface —
/// the driver's own way of saying "you are painting at the wrong size". See
/// [`report_present_result`].
const VK_SUBOPTIMAL_KHR: i32 = 1_000_001_003;
const VK_ERROR_OUT_OF_DATE_KHR: i32 = -1_000_001_004;

// -------------------------------------------------------------- host loading

/// The real entry points, resolved from the host's Vulkan loader once.
struct HostVulkan {
    get_instance_proc_addr: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
    create_instance:
        unsafe extern "C" fn(*const VkInstanceCreateInfo, *const c_void, *mut *mut c_void) -> i32,
    enumerate_instance_extension_properties:
        unsafe extern "C" fn(*const c_char, *mut u32, *mut VkExtensionProperties) -> i32,
}

// Only ever read after `OnceLock` initialisation; the fields are plain function
// pointers into a library that, like every other host library this runtime
// opens, is never closed.
unsafe impl Send for HostVulkan {}
unsafe impl Sync for HostVulkan {}

static HOST: OnceLock<Option<HostVulkan>> = OnceLock::new();

fn host() -> Option<&'static HostVulkan> {
    HOST.get_or_init(load_host).as_ref()
}

extern "C" {
    // The *host* loader, not the bionic one — same reasoning as `window.rs`'s
    // X11 loading and `symtab.rs`'s `host_dlopen`: this file must reach real
    // `/usr/lib64/libvulkan.so.1`, and the bionic `dlopen` Roblox itself calls is
    // the one this module exists to answer, not to recurse through.
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}
use std::ffi::c_int;
const RTLD_NOW: c_int = 2;

fn load_host() -> Option<HostVulkan> {
    // `CORDIAL_NO_VULKAN=1` makes the host look exactly like a machine with no
    // Vulkan loader at all: `host()` returns `None`, `symtab::build` leaves both
    // virtual `libvulkan.so`/`libvulkan.so.1` sonames unregistered, and Roblox's
    // own `dlopen` fails the same way it did before this module existed — a
    // clean, deliberate fall-through to GLES. Useful on its own (forcing the
    // fallback path to test it) independent of whatever bug prompted adding it.
    //
    // Kept as its own switch even though the Graphics setting now reaches the
    // same state, because it is the control: it answers "is this the backend or
    // the setting" without going near the resolution in `crate::graphics`.
    if std::env::var_os("CORDIAL_NO_VULKAN").is_some() {
        return None;
    }

    // The Graphics setting, and the plugin layer behind it when the user left it
    // on Automatic. `GlEs` withholds the loader by the same route the switch
    // above takes — there is no separate GLES code path to select, because the
    // engine already has one and picks it when Vulkan is not there.
    if !crate::graphics::choice().backend.offers_vulkan() {
        return None;
    }

    let mut handle = std::ptr::null_mut();
    // The Linux soname first, then the Android one Roblox actually asks for —
    // either is fine to load from, since what matters is which real library
    // answers, not which name found it.
    for name in [c"libvulkan.so.1", c"libvulkan.so"] {
        // SAFETY: literal, NUL-terminated sonames; the handle is never closed.
        handle = unsafe { dlopen(name.as_ptr(), RTLD_NOW) };
        if !handle.is_null() {
            break;
        }
    }
    if handle.is_null() {
        return None;
    }

    // SAFETY: `handle` is open; the name is the Vulkan loader's own
    // documented export.
    let gipa = unsafe { dlsym(handle, c"vkGetInstanceProcAddr".as_ptr()) };
    if gipa.is_null() {
        return None;
    }
    // SAFETY: resolved from the host loader for exactly this name.
    let gipa: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void =
        unsafe { std::mem::transmute(gipa) };

    // `vkCreateInstance` and `vkEnumerateInstanceExtensionProperties` are the
    // two "global" commands Roblox needs before any `VkInstance` exists. Per
    // spec they are only guaranteed reachable through `vkGetInstanceProcAddr(
    // NULL, name)` — the Linux loader also exports them directly, but going
    // through the documented bootstrap costs nothing extra and is the same path
    // being interposed for everything else.
    // SAFETY: `gipa` came from the host loader and `instance = NULL` is its
    // documented way to ask for global commands.
    let create_instance = unsafe { gipa(std::ptr::null_mut(), c"vkCreateInstance".as_ptr()) };
    let enum_ext = unsafe {
        gipa(
            std::ptr::null_mut(),
            c"vkEnumerateInstanceExtensionProperties".as_ptr(),
        )
    };
    if create_instance.is_null() || enum_ext.is_null() {
        return None;
    }

    Some(HostVulkan {
        get_instance_proc_addr: gipa,
        // SAFETY: resolved from the host loader for exactly these names.
        create_instance: unsafe { std::mem::transmute(create_instance) },
        enumerate_instance_extension_properties: unsafe { std::mem::transmute(enum_ext) },
    })
}

// --------------------------------------------------------------- registration

/// The sonames Roblox tries, in the order it tries them (per
/// `docs/framework-api-inventory.md`). Both are registered identically —
/// whichever one bionic's `dlopen` is asked for finds the same virtual library.
pub const LIBRARY_NAMES: [&str; 2] = ["libvulkan.so", "libvulkan.so.1"];

/// The one export the virtual `libvulkan.so`/`libvulkan.so.1` libraries need.
/// `None` if the host has no Vulkan at all, in which case `symtab::build` leaves
/// both sonames unregistered and Roblox's `dlopen` fails exactly as it does
/// today — a clean fall-through to GLES, not a half-working Vulkan.
pub fn get_instance_proc_addr_symbol() -> Option<*mut c_void> {
    host().map(|_| vk_get_instance_proc_addr as *const () as *mut c_void)
}

// -------------------------------------------------------------- interposition

/// The engine's `VkInstance`, kept because instance-level commands cannot be
/// resolved without one.
///
/// Recorded here rather than in `vk_create_instance` because this function
/// sees it on every later lookup and the creation path has several returns;
/// one store on a hot-ish path is cheaper than another way to miss it. The
/// capture in `take_capture` needs `vkGetPhysicalDeviceMemoryProperties`,
/// which a null instance does not resolve -- that mistake cost a run whose log
/// said only "driver has no vkGetPhysicalDeviceMemoryProperties", which is
/// true of every driver when you ask without an instance.
static INSTANCE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

extern "C" fn vk_get_instance_proc_addr(instance: *mut c_void, name: *const c_char) -> *mut c_void {
    if !instance.is_null() {
        INSTANCE.store(instance as usize, std::sync::atomic::Ordering::Relaxed);
    }
    let Some(h) = host() else {
        return std::ptr::null_mut();
    };
    if name.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: bionic's `vkGetInstanceProcAddr` contract is a NUL-terminated name.
    let bytes = unsafe { CStr::from_ptr(name) }.to_bytes();
    // The full ordered list of names resolved here is identical between the X11
    // and Wayland runs (checked directly, byte for byte) — Roblox builds one
    // static dispatch table regardless of backend, so *which* names get
    // resolved says nothing about which ones are actually called. Do not re-add
    // a trace here; it was tried and produced no signal. What answers the
    // question is instrumenting the handful of WSI calls themselves, below.
    match bytes {
        // A `vkGetInstanceProcAddr(instance, "vkGetInstanceProcAddr")` query
        // must return itself — the spec requires it, and code that
        // self-verifies its loader this way exists.
        b"vkGetInstanceProcAddr" => vk_get_instance_proc_addr as *const () as *mut c_void,
        b"vkCreateInstance" => vk_create_instance as *const () as *mut c_void,
        b"vkEnumerateInstanceExtensionProperties" => {
            vk_enumerate_instance_extension_properties as *const () as *mut c_void
        }
        b"vkCreateAndroidSurfaceKHR" => vk_create_android_surface_khr as *const () as *mut c_void,
        // Counted, not altered. A Vulkan session leaves every GLES counter at
        // zero, so without this the graphics report cannot tell "Vulkan is
        // presenting frames" from "nothing is drawing at all".
        // Device-level entry points are normally fetched through
        // `vkGetDeviceProcAddr`, not through this function — that is the whole
        // point of the device dispatch. Intercepting only the instance getter
        // meant the present counter never incremented and a perfectly healthy
        // Vulkan session read as "nothing is drawing".
        b"vkGetDeviceProcAddr" => {
            HOST_GET_DEVICE_PROC_ADDR.store(
                unsafe { (h.get_instance_proc_addr)(instance, name) } as usize,
                std::sync::atomic::Ordering::Relaxed,
            );
            vk_get_device_proc_addr as *const () as *mut c_void
        }
        b"vkQueuePresentKHR" => {
            HOST_QUEUE_PRESENT.store(
                unsafe { (h.get_instance_proc_addr)(instance, name) } as usize,
                std::sync::atomic::Ordering::Relaxed,
            );
            vk_queue_present_khr as *const () as *mut c_void
        }
        // Interposed for its *first argument* and nothing else. Asking the
        // driver which present modes a surface supports needs a
        // `VkPhysicalDevice`, and `vkCreateSwapchainKHR` is handed a
        // `VkDevice`; this is the one call that sees both, so it is where the
        // association is recorded. See [`vk_create_device`].
        b"vkCreateDevice" => {
            HOST_CREATE_DEVICE.store(
                unsafe { (h.get_instance_proc_addr)(instance, name) } as usize,
                std::sync::atomic::Ordering::Relaxed,
            );
            vk_create_device as *const () as *mut c_void
        }
        // A device-level command, normally fetched through
        // `vkGetDeviceProcAddr` and peeled off there. It is answered here too
        // because `vkGetInstanceProcAddr` is required to answer for device
        // commands as well, and a client that used that route would otherwise
        // get the host's unpatched entry point and silently stay on FIFO —
        // which is exactly the class of "the switch appears to work and does
        // nothing" this file already has one comment about.
        b"vkCreateSwapchainKHR" => {
            HOST_CREATE_SWAPCHAIN.store(
                unsafe { (h.get_instance_proc_addr)(instance, name) } as usize,
                std::sync::atomic::Ordering::Relaxed,
            );
            vk_create_swapchain_khr as *const () as *mut c_void
        }
        // `vkGetPhysicalDeviceSurfaceCapabilitiesKHR`'s result is patched, not
        // just forwarded — see [`vk_get_physical_device_surface_capabilities_khr`]
        // for the failure this fixes. Measured, not guessed: instrumenting this
        // call (and `vkCreateSwapchainKHR`/`vkAcquireNextImageKHR`, since
        // reverted — the finding is what matters, not the scaffolding) showed
        // `currentExtent` coming back as `4294967295x4294967295` on Wayland and
        // a real `1280x720` on X11 for the identical query, and zero calls to
        // `vkCreateSwapchainKHR` ever following it on Wayland, against one
        // (and 653 to `vkAcquireNextImageKHR`) on X11 in the same window.
        b"vkGetPhysicalDeviceSurfaceCapabilitiesKHR" => {
            HOST_GET_SURFACE_CAPS.store(
                unsafe { (h.get_instance_proc_addr)(instance, name) } as usize,
                std::sync::atomic::Ordering::Relaxed,
            );
            vk_get_physical_device_surface_capabilities_khr as *const () as *mut c_void
        }
        // Every other name — `vkCreateDevice`, every `vkCmd*`, everything a real
        // `VkInstance` answers for once one exists — is exactly what the host
        // loader would give a native Linux Vulkan app. Forwarding unconditionally
        // is correct because `instance`, once created, is a real host
        // `VkInstance`: see `vk_create_instance`.
        _ => unsafe { (h.get_instance_proc_addr)(instance, name) },
    }
}

/// The host's `vkGetDeviceProcAddr`, so device-level lookups can be forwarded
/// after the counted ones are peeled off.
static HOST_GET_DEVICE_PROC_ADDR: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

extern "C" fn vk_get_device_proc_addr(device: *mut c_void, name: *const c_char) -> *mut c_void {
    let f = HOST_GET_DEVICE_PROC_ADDR.load(std::sync::atomic::Ordering::Relaxed);
    if f == 0 || name.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: resolved from the host loader for exactly this name.
    let host: extern "C" fn(*mut c_void, *const c_char) -> *mut c_void =
        unsafe { std::mem::transmute(f) };
    // SAFETY: Vulkan's contract is a NUL-terminated name.
    match unsafe { CStr::from_ptr(name) }.to_bytes() {
        b"vkQueuePresentKHR" => {
            HOST_QUEUE_PRESENT.store(host(device, name) as usize, std::sync::atomic::Ordering::Relaxed);
            vk_queue_present_khr as *const () as *mut c_void
        }
        b"vkCreateSwapchainKHR" => {
            HOST_CREATE_SWAPCHAIN
                .store(host(device, name) as usize, std::sync::atomic::Ordering::Relaxed);
            vk_create_swapchain_khr as *const () as *mut c_void
        }
        _ => host(device, name),
    }
}

/// The real `vkQueuePresentKHR`, resolved on first request and then called
/// through unchanged.
static HOST_QUEUE_PRESENT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

extern "C" fn vk_queue_present_khr(queue: *mut c_void, info: *const c_void) -> i32 {
    crate::android::glcount::QUEUE_PRESENT
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // How evenly, not just how often. A count is a mean and a mean hides
    // judder -- see `frame_pacing`.
    crate::android::frame_pacing::record_present();
    let f = HOST_QUEUE_PRESENT.load(std::sync::atomic::Ordering::Relaxed);
    if f == 0 {
        return 0;
    }
    // A capture, if one was asked for, before the frame goes anywhere.
    //
    // Here rather than after the present because this is the only moment the
    // image is both complete and in a layout the copy can name: the engine has
    // finished rendering it and left it in `VK_IMAGE_LAYOUT_PRESENT_SRC_KHR`.
    // Once the present has been forwarded the image belongs to the driver and
    // reading it is a race.
    if super::capture::pending() {
        take_capture(queue, info);
    }
    // SAFETY: resolved from the host loader for exactly this name.
    let f: extern "C" fn(*mut c_void, *const c_void) -> i32 = unsafe { std::mem::transmute(f) };
    let rc = f(queue, info);
    if rc != VK_SUCCESS {
        report_present_result(rc);
    }
    rc
}

/// Pull the image index out of `VkPresentInfoKHR` and hand the copy off.
///
/// `pImageIndices` sits 48 bytes in, by the specification's C layout, after
/// `sType`, `pNext`, the wait-semaphore pair and the swapchain pair. Read
/// unaligned because nothing guarantees the caller's struct alignment matches
/// ours, and a misaligned read is undefined behaviour rather than a slow one.
fn take_capture(queue: *mut c_void, info: *const c_void) {
    if info.is_null() {
        return;
    }
    // SAFETY: the caller passes a valid `VkPresentInfoKHR`; only the image
    // index array is read, and only its first element, which the count above
    // it guarantees exists for any present that names a swapchain.
    // Take BOTH the swapchain and the image index from the present itself.
    //
    // The first version took only the index here and used the swapchain
    // recorded at creation time, which is wrong whenever the engine presents on
    // anything other than the most recently created one -- after a recreate, or
    // with more than one alive. The image then came from a swapchain that had
    // been created and never drawn into, so every capture was the clear colour:
    // a uniform field, identical to six decimal places between runs that looked
    // nothing alike on screen. That was very nearly written up as "Roblox
    // presents blank frames".
    //
    // `pSwapchains` is at offset 40 and `pImageIndices` at 48, by the
    // specification's C layout: sType, pNext, the wait-semaphore pair, then the
    // swapchain count and array, then the indices. Read unaligned because
    // nothing guarantees the caller's struct shares our alignment.
    let (swapchain, image_index) = unsafe {
        let base = info as *const u8;
        let count = std::ptr::read_unaligned(base.add(32) as *const u32);
        let chains = std::ptr::read_unaligned(base.add(40) as *const *const u64);
        let indices = std::ptr::read_unaligned(base.add(48) as *const *const u32);
        if count == 0 || chains.is_null() || indices.is_null() {
            return;
        }
        (std::ptr::read_unaligned(chains), std::ptr::read_unaligned(indices))
    };
    let Some(h) = host() else { return };
    let gdpa = HOST_GET_DEVICE_PROC_ADDR.load(std::sync::atomic::Ordering::Relaxed);
    if gdpa == 0 {
        println!("[android] vulkan: capture needs vkGetDeviceProcAddr and it was never resolved");
        return;
    }
    let phys = PHYSICAL_DEVICE.load(std::sync::atomic::Ordering::Relaxed) as *mut c_void;
    if phys.is_null() {
        println!("[android] vulkan: capture needs the physical device and vkCreateDevice never ran");
        return;
    }
    let name = c"vkGetPhysicalDeviceMemoryProperties";
    let inst = INSTANCE.load(std::sync::atomic::Ordering::Relaxed) as *mut c_void;
    // SAFETY: the instance getter is the host's, called with the engine's own
    // instance, for a core instance-level command.
    let gpdmp = unsafe { (h.get_instance_proc_addr)(inst, name.as_ptr()) };
    if gpdmp.is_null() {
        // Once. This runs inside a present, so a per-frame complaint would
        // bury the run at sixty lines a second -- which is exactly what the
        // first version of this did.
        static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !SAID.swap(true, std::sync::atomic::Ordering::Relaxed) {
            println!("[android] vulkan: no vkGetPhysicalDeviceMemoryProperties, so no capture");
        }
        super::capture::abandon("vkGetPhysicalDeviceMemoryProperties is unavailable");
        return;
    }
    // SAFETY: both were resolved from the loader for exactly these names, and
    // are called with the signatures the specification gives them.
    unsafe {
        super::capture::capture(
            queue as u64,
            swapchain,
            image_index,
            std::mem::transmute::<usize, extern "C" fn(u64, *const c_char) -> *mut c_void>(gdpa),
            std::mem::transmute::<
                *mut c_void,
                extern "C" fn(*mut c_void, *mut super::capture::PhysicalDeviceMemoryProperties),
            >(gpdmp),
            phys,
        );
    }
}

/// Ask for the next presented frame to be written to `path`, and wait for it.
///
/// Blocking, with a bound, because the caller is a socket handler answering a
/// harness that wants the file to exist by the time it is told `ok`. The bound
/// matters more than the wait: a client whose engine has stopped presenting --
/// which is the exact failure this whole surface was built to investigate --
/// would otherwise hang the harness that came to look at it.
pub fn request_capture(path: &str) -> Result<String, String> {
    super::capture::request(path);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if let Some(r) = super::capture::take_result() {
            return r;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    Err("no frame was presented within three seconds (the engine may be wedged)".into())
}

/// The extent of the most recent swapchain, for callers reporting state.
pub fn last_extent() -> (u32, u32) {
    super::capture::extent()
}

/// Say when a present came back as anything other than `VK_SUCCESS`, at the
/// first one of each code and then at each power of ten.
///
/// Cordial forwards this return value untouched and always has, so it is not
/// swallowed here — but "not swallowed" and "acted on" are different claims and
/// nothing could tell them apart from outside. `VK_SUBOPTIMAL_KHR` and
/// `VK_ERROR_OUT_OF_DATE_KHR` are how the driver says the swapchain no longer
/// matches the surface, which is exactly the shape of the fullscreen bug where
/// the engine keeps painting at its old size in a new-sized slot: an engine
/// that ignores them would look identical to a Cordial that ate them. Now the
/// log says which.
///
/// Counted rather than printed per call for the reason `input::
/// report_unregistered` gives: one line would be indistinguishable from a
/// transient at a resize, and one per frame would bury the run.
fn report_present_result(rc: i32) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SUBOPTIMAL: AtomicU64 = AtomicU64::new(0);
    static OUT_OF_DATE: AtomicU64 = AtomicU64::new(0);
    static OTHER: AtomicU64 = AtomicU64::new(0);
    let (counter, name) = match rc {
        VK_SUBOPTIMAL_KHR => (&SUBOPTIMAL, "VK_SUBOPTIMAL_KHR"),
        VK_ERROR_OUT_OF_DATE_KHR => (&OUT_OF_DATE, "VK_ERROR_OUT_OF_DATE_KHR"),
        _ => (&OTHER, "a non-success code"),
    };
    let n = counter.fetch_add(1, Ordering::Relaxed) + 1;
    if is_first_or_power_of_ten(n) {
        println!("[android] vulkan: vkQueuePresentKHR returned {name} ({rc}), {n} so far");
    }
}

/// Whether a count is the first or a power of ten, the reporting cadence this
/// file and `input::report_unregistered` share.
///
/// A free function rather than a trait on `u64`: the trait bought nothing and
/// put a method with a name like this on every integer in the module.
fn is_first_or_power_of_ten(n: u64) -> bool {
    let mut p = 1u64;
    while p < n {
        let next = p.saturating_mul(10);
        if next == p {
            return false;
        }
        p = next;
    }
    p == n
}

// ------------------------------------------------------------- present mode
//
// Cordial did not ask for a present mode until this existed, so the engine's
// own choice stood, and the engine chooses `VK_PRESENT_MODE_FIFO_KHR` — the one
// mode the specification guarantees, and a hard vsync lock. What that costs was
// measured rather than assumed: with input driven continuously for the whole
// window, presents come out equal to the output's refresh and stay there, 60.0
// on a 59.88 Hz panel and 49.4 on a 49.96 Hz one, unchanged by fullscreen at
// four times the pixels. Sober, on the same machine and the same APK, reports
// above both, so the ceiling is FIFO and not the engine.
//
// Read AGENTS.md's "Do not use present counts as a frame rate" before repeating
// any of that. Presents fall to exactly 1.0 a second after about thirteen idle
// seconds, so a count taken over a window with no input flowing measures the
// idle throttle and nothing else.

/// The `VkInstance` this shim created, so instance-level commands can be
/// resolved later — [`supported_present_modes`] needs
/// `vkGetPhysicalDeviceSurfacePresentModesKHR`, which cannot be fetched with a
/// null instance.
static HOST_INSTANCE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The `VkPhysicalDevice` the engine created its logical device from, recorded
/// by [`vk_create_device`].
///
/// One slot, not a map. The engine creates exactly one logical device, and
/// nothing in this runtime has ever seen it create a second; a map keyed by
/// `VkDevice` would be more code defending against a case that would also need
/// a second window to be worth anything. If a build ever does create two, the
/// consequence is bounded — the present-mode query would be asked of the wrong
/// physical device and would answer for a GPU that is not rendering, which
/// shows up as MAILBOX being offered where it is not supported and
/// `vkCreateSwapchainKHR` failing loudly rather than silently.
static PHYSICAL_DEVICE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

static HOST_CREATE_DEVICE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// `vkCreateDevice`, forwarded byte for byte after noting which physical device
/// it was called against. Nothing else about it is touched, and deliberately so:
/// `VkDeviceCreateInfo` carries queue priorities, enabled features and an
/// extension list that this file has no business having an opinion about.
extern "C" fn vk_create_device(
    physical_device: *mut c_void,
    create_info: *const c_void,
    allocator: *const c_void,
    device_out: *mut *mut c_void,
) -> i32 {
    let f = HOST_CREATE_DEVICE.load(std::sync::atomic::Ordering::Relaxed);
    if f == 0 {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    PHYSICAL_DEVICE.store(physical_device as usize, std::sync::atomic::Ordering::Relaxed);
    // The queue family the engine asked for, read out of its own
    // `VkDeviceCreateInfo`. A capture needs a command pool, a command pool is
    // per family, and assuming family zero would be a guess that happens to be
    // right on this driver and wrong on the next one. Offsets are the
    // specification's C layout: `queueCreateInfoCount` at 20, the array
    // pointer at 24, and `queueFamilyIndex` 20 bytes into the first element.
    if !create_info.is_null() {
        // SAFETY: the caller passes a valid `VkDeviceCreateInfo`, and only the
        // two documented fields are read.
        unsafe {
            let base = create_info as *const u8;
            let count = std::ptr::read_unaligned(base.add(20) as *const u32);
            let infos = std::ptr::read_unaligned(base.add(24) as *const *const u8);
            if count > 0 && !infos.is_null() {
                let family = std::ptr::read_unaligned(infos.add(20) as *const u32);
                super::capture::QUEUE_FAMILY.store(family as u64, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }
    type Fn_ = extern "C" fn(*mut c_void, *const c_void, *const c_void, *mut *mut c_void) -> i32;
    // SAFETY: resolved from the host loader for exactly this name, and called
    // with the caller's own arguments unchanged.
    let f: Fn_ = unsafe { std::mem::transmute(f) };
    f(physical_device, create_info, allocator, device_out)
}

/// What the present-mode setting asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PresentModeChoice {
    /// Forward whatever the engine put in `VkSwapchainCreateInfoKHR`. This is
    /// the control for a measurement: `CORDIAL_PRESENT_MODE=off` reproduces the
    /// behaviour of every build before this code existed, in the same session,
    /// which is what AGENTS.md asks a timing claim to come with.
    Untouched,
    /// The first of these the driver advertises for this surface wins;
    /// if it advertises none of them, the engine's own choice stands.
    Prefer(&'static [i32]),
}

/// The flag-layer key that asks for a present mode, and the environment
/// variable that overrides it.
///
/// A `Cordial`-prefixed key rides `flags.rs`'s layering for its precedence and
/// provenance, and `client_settings.rs`'s `is_roblox_flag` filters it back out
/// before anything reaches Roblox's settings document — the engine has no idea
/// this key exists. Exactly the arrangement `graphics.rs`'s
/// `CordialGraphicsBackend` already uses, deliberately, rather than a second
/// way of doing the same thing.
///
/// **This is what makes "unlock the frame rate" available to a plugin without
/// giving a plugin the swapchain.** A plugin writes a value into its own flags
/// layer with `flags.set`; Cordial reads it here and decides what to hand
/// `vkCreateSwapchainKHR`. The plugin never learns that a swapchain exists,
/// cannot name a mode the driver does not advertise, and cannot reach any other
/// Vulkan call — the effect, never the channel (ADR-007). It also inherits the
/// layering's own answer to "who wins": the user's `flags.json` beats every
/// plugin's, so a plugin cannot quietly overrule a mode somebody chose.
pub const PRESENT_MODE_KEY: &str = "CordialPresentMode";

/// `CORDIAL_PRESENT_MODE=off|auto|mailbox|immediate|fifo|fifo-relaxed`.
pub const PRESENT_MODE_ENV: &str = "CORDIAL_PRESENT_MODE";

/// One spelling of a present mode, or `None` if it is not one.
///
/// `None` rather than a fallback, so [`resolve_present_mode`] can say which
/// setting was unreadable. `graphics.rs` makes the same split for the same
/// reason: a switch that looks set and silently does nothing is the failure
/// this project keeps finding.
fn parse_present_mode(text: &str) -> Option<PresentModeChoice> {
    match text.trim().to_ascii_lowercase().as_str() {
        "" | "auto" => Some(PresentModeChoice::Prefer(&[VK_PRESENT_MODE_MAILBOX_KHR])),
        "off" | "engine" => Some(PresentModeChoice::Untouched),
        "mailbox" => Some(PresentModeChoice::Prefer(&[VK_PRESENT_MODE_MAILBOX_KHR])),
        "immediate" => Some(PresentModeChoice::Prefer(&[VK_PRESENT_MODE_IMMEDIATE_KHR])),
        // "As fast as the driver will let me, tearing if that is what it
        // takes." MAILBOX first because it uncaps without tearing where the
        // driver has it; IMMEDIATE is the fallback that always exists. One
        // name for the intent, so a plugin asking for an uncapped frame rate
        // does not have to know which modes this surface advertises.
        "uncapped" => Some(PresentModeChoice::Prefer(&[
            VK_PRESENT_MODE_MAILBOX_KHR,
            VK_PRESENT_MODE_IMMEDIATE_KHR,
        ])),
        "fifo" => Some(PresentModeChoice::Prefer(&[VK_PRESENT_MODE_FIFO_KHR])),
        "fifo-relaxed" | "fifo_relaxed" => {
            Some(PresentModeChoice::Prefer(&[VK_PRESENT_MODE_FIFO_RELAXED_KHR]))
        }
        _ => None,
    }
}

/// The spellings a setting may use, for an error message that lists them.
const PRESENT_MODE_NAMES: &str = "off, auto, mailbox, immediate, uncapped, fifo, fifo-relaxed";

/// Which present mode is in force, and who asked for it.
///
/// Environment first, then the flag layers, then `auto` — the order
/// `graphics.rs::resolve` uses, and for its reason: the environment variable is
/// what a measurement run and the shell both set, so it has to be able to
/// overrule a plugin that was installed to do something else.
///
/// Pure, so the precedence can be tested without a machine that has a Vulkan
/// driver on it.
fn resolve_present_mode(
    from_env: Option<String>,
    from_flags: Option<(String, String)>,
) -> (PresentModeChoice, String) {
    let auto = PresentModeChoice::Prefer(&[VK_PRESENT_MODE_MAILBOX_KHR]);

    if let Some(text) = from_env.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        match parse_present_mode(text) {
            // An explicit `auto` from the environment still lets a plugin have
            // its say, the same way `graphics.rs` treats `automatic`: the user
            // said "you decide", not "ignore everything".
            Some(choice) if choice == auto && text.eq_ignore_ascii_case("auto") => {}
            Some(choice) => return (choice, format!("{PRESENT_MODE_ENV}={text}")),
            None => {
                println!(
                    "[android] vulkan: {PRESENT_MODE_ENV}={text:?} is not a present mode; \
                     using auto. Known: {PRESENT_MODE_NAMES}"
                );
                return (auto, format!("auto, after an unusable {PRESENT_MODE_ENV}"));
            }
        }
    }

    if let Some((source, text)) = from_flags {
        match parse_present_mode(&text) {
            Some(choice) => return (choice, format!("{PRESENT_MODE_KEY}={text} from {source}")),
            None => {
                println!(
                    "[android] vulkan: {PRESENT_MODE_KEY}={text:?} from {source} is not a \
                     present mode; using auto. Known: {PRESENT_MODE_NAMES}"
                );
                return (auto, format!("auto, after an unusable {PRESENT_MODE_KEY}"));
            }
        }
    }

    (auto, "auto".to_string())
}

/// What the flag layers say about [`PRESENT_MODE_KEY`], if anything.
fn present_mode_from_flags() -> Option<(String, String)> {
    let resolved = crate::flags::resolve(crate::flags::collect());
    let entry = resolved.get(PRESENT_MODE_KEY)?;
    Some((entry.source.describe(), entry.value.clone()))
}

/// The present mode this process will ask for, decided once.
///
/// **`auto` is MAILBOX. It was briefly FIFO, and that was measured wrong.**
///
/// The case for FIFO is real and still stands on its own terms: MAILBOX draws
/// every frame the GPU can produce and throws away the ones the display never
/// scans out, so on a 60 Hz panel a scene the GPU could render at 300 fps burns
/// several times the power to show the same sixty frames. On a handheld that is
/// battery and on a laptop it is the fan. FIFO is also the only mode the
/// specification guarantees.
///
/// What that argument left out is latency, and a user found it within the hour:
/// "the mouse feels floaty and weird in roblox", then, with the control run,
/// "switching back to Mailbox fixes the floaty fealing". FIFO queues presents
/// against the display clock, so the cursor and the camera lag the hand by up
/// to the queue depth. MAILBOX has no queue to wait behind -- it replaces the
/// pending image -- which is the whole of the difference in feel.
///
/// **The power argument was reasoned and the latency one was measured**, which
/// is the order this project settles disagreements in. Power is still real, and
/// `fifo` is one row away in Settings, named as the setting that matches the
/// display; the difference is that nobody now pays for it without choosing it.
///
/// Decided once and not re-read: the mode is a field of
/// `VkSwapchainCreateInfoKHR`, so changing it means a new swapchain, and the
/// engine owns when that happens. A setting that appeared to change live and
/// only took effect at the next resize would be worse than one that plainly
/// takes effect at the next launch.
fn present_mode_choice() -> PresentModeChoice {
    static CHOICE: OnceLock<PresentModeChoice> = OnceLock::new();
    *CHOICE.get_or_init(|| {
        let (choice, source) =
            resolve_present_mode(std::env::var(PRESENT_MODE_ENV).ok(), present_mode_from_flags());
        println!("[android] vulkan: present mode setting is {source}");
        choice
    })
}

/// What the driver says it can do with this surface, or `None` if the question
/// could not be asked.
///
/// `None` is not "nothing is supported" — it means the query failed, and the
/// caller leaves the engine's own present mode alone rather than substituting
/// one on a guess. A shim that offered MAILBOX because it could not find out
/// whether MAILBOX exists would be the stub-that-lies AGENTS.md rules out, one
/// layer up.
fn supported_present_modes(surface: u64) -> Option<Vec<i32>> {
    let h = host()?;
    let instance = HOST_INSTANCE.load(std::sync::atomic::Ordering::Relaxed);
    let physical_device = PHYSICAL_DEVICE.load(std::sync::atomic::Ordering::Relaxed);
    if instance == 0 || physical_device == 0 {
        return None;
    }
    // SAFETY: `instance` is the real host `VkInstance` this shim created, and
    // the name is the WSI extension's own documented export.
    let f = unsafe {
        (h.get_instance_proc_addr)(
            instance as *mut c_void,
            c"vkGetPhysicalDeviceSurfacePresentModesKHR".as_ptr(),
        )
    };
    if f.is_null() {
        return None;
    }
    type Fn_ = extern "C" fn(*mut c_void, u64, *mut u32, *mut i32) -> i32;
    // SAFETY: resolved from the host loader for exactly this name.
    let f: Fn_ = unsafe { std::mem::transmute(f) };

    // The two-call idiom, the same one
    // `vk_enumerate_instance_extension_properties` runs against the host above.
    let mut count: u32 = 0;
    if f(physical_device as *mut c_void, surface, &mut count, std::ptr::null_mut()) != VK_SUCCESS {
        return None;
    }
    let mut modes = vec![0i32; count as usize];
    if count > 0
        && f(physical_device as *mut c_void, surface, &mut count, modes.as_mut_ptr()) != VK_SUCCESS
    {
        return None;
    }
    modes.truncate(count as usize);
    Some(modes)
}

/// What the driver will let this surface hold, as `(minImageCount,
/// maxImageCount)`, with `0` for max meaning "no limit" exactly as the
/// specification defines it.
///
/// `None` on the same terms as [`supported_present_modes`]: the question could
/// not be asked, so the caller changes nothing rather than guessing.
fn surface_image_count_limits(surface: u64) -> Option<(u32, u32)> {
    let h = host()?;
    let instance = HOST_INSTANCE.load(std::sync::atomic::Ordering::Relaxed);
    let physical_device = PHYSICAL_DEVICE.load(std::sync::atomic::Ordering::Relaxed);
    if instance == 0 || physical_device == 0 {
        return None;
    }
    // SAFETY: `instance` is the real host `VkInstance` this shim created, and
    // the name is the WSI extension's own documented export.
    let f = unsafe {
        (h.get_instance_proc_addr)(
            instance as *mut c_void,
            c"vkGetPhysicalDeviceSurfaceCapabilitiesKHR".as_ptr(),
        )
    };
    if f.is_null() {
        return None;
    }
    type Fn_ = extern "C" fn(*mut c_void, u64, *mut VkSurfaceCapabilitiesKHR) -> i32;
    // SAFETY: resolved from the host loader for exactly this name.
    let f: Fn_ = unsafe { std::mem::transmute(f) };
    let mut caps = VkSurfaceCapabilitiesKHR {
        min_image_count: 0,
        max_image_count: 0,
        current_extent: VkExtent2D { width: 0, height: 0 },
        min_image_extent: VkExtent2D { width: 0, height: 0 },
        max_image_extent: VkExtent2D { width: 0, height: 0 },
        max_image_array_layers: 0,
        supported_transforms: 0,
        current_transform: 0,
        supported_composite_alpha: 0,
        supported_usage_flags: 0,
    };
    if f(physical_device as *mut c_void, surface, &mut caps) != VK_SUCCESS {
        return None;
    }
    Some((caps.min_image_count, caps.max_image_count))
}

static HOST_CREATE_SWAPCHAIN: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// `vkCreateSwapchainKHR`, with `presentMode` substituted when the driver has
/// something better than what the engine asked for.
///
/// **This overrides a choice the engine made explicitly, which is a decision
/// and not a translation.** Everything else in this file substitutes one
/// spelling of the same thing for another — an Android surface for a Wayland
/// one, an extension name for the name the same capability has on desktop. This
/// does not: FIFO is a mode the engine named, the driver implements, and would
/// otherwise get. The argument for doing it anyway is that FIFO is also the
/// mode a client picks when it cannot find out what else exists, which is the
/// position Roblox's Android renderer is in — Android has had MAILBOX since
/// Vulkan shipped, but a phone's power budget makes vsync-locked the right
/// default there and a desktop's does not. The engine has no setting for this
/// and no way to learn it is on a desktop, so the choice has to be made
/// somewhere or not at all.
///
/// Reversible without recompiling, and the reversal is a supported
/// configuration rather than a debugging leftover: `CORDIAL_PRESENT_MODE=off`
/// forwards the engine's own choice untouched. The chosen mode is printed
/// unconditionally for the same reason `backend()` prints which display backend
/// it picked — "what did Cordial substitute" is exactly what a bug report about
/// tearing or stutter needs, and a trace flag would hide it.
///
/// FIFO stays the fallback on every path through here, because FIFO is the only
/// present mode the specification requires an implementation to support. A
/// client that assumes MAILBOX exists works on Mesa and breaks on some drivers.
/// Records what a capture needs, then creates the swapchain as before.
///
/// A wrapper rather than an edit to each of the inner function's several early
/// returns, so that "what was created" is recorded in exactly one place no
/// matter which path produced it. The alternative reliably grows a route that
/// forgets, and a capture against a stale swapchain handle is a driver crash
/// rather than a wrong picture.
extern "C" fn vk_create_swapchain_khr(
    device: *mut c_void,
    create_info: *const VkSwapchainCreateInfoKHR,
    allocator: *const c_void,
    swapchain_out: *mut u64,
) -> i32 {
    let rc = vk_create_swapchain_inner(device, create_info, allocator, swapchain_out);
    if rc == VK_SUCCESS && !create_info.is_null() && !swapchain_out.is_null() {
        // SAFETY: both pointers are the caller's, checked for null, and the
        // driver has just reported success so `swapchain_out` is written.
        unsafe {
            let info = &*create_info;
            super::capture::DEVICE.store(device as usize, std::sync::atomic::Ordering::Relaxed);
            super::capture::note_swapchain(
                *swapchain_out,
                info.image_extent.width,
                info.image_extent.height,
                info.image_format as u32,
            );
        }
    }
    rc
}

extern "C" fn vk_create_swapchain_inner(
    device: *mut c_void,
    create_info: *const VkSwapchainCreateInfoKHR,
    allocator: *const c_void,
    swapchain_out: *mut u64,
) -> i32 {
    let f = HOST_CREATE_SWAPCHAIN.load(std::sync::atomic::Ordering::Relaxed);
    if f == 0 {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    type Fn_ =
        extern "C" fn(*mut c_void, *const VkSwapchainCreateInfoKHR, *const c_void, *mut u64) -> i32;
    // SAFETY: resolved from the host loader for exactly this name.
    let f: Fn_ = unsafe { std::mem::transmute(f) };

    // SAFETY: `create_info` is the caller's own `VkSwapchainCreateInfoKHR`; a
    // null one is the caller's bug, forwarded so the host reports it on its own
    // terms rather than being masked here.
    let Some(info) = (unsafe { create_info.as_ref() }) else {
        return f(device, create_info, allocator, swapchain_out);
    };
    // The extent, unconditionally and on the same argument the present mode is
    // printed on: when the client is letterboxed on one side and clipped on the
    // other, the first thing worth knowing is what size the swapchain it is
    // painting into was built at, and whether it was ever built again. It is one
    // line per swapchain, not per frame.
    println!(
        "[android] vulkan: vkCreateSwapchainKHR extent {}x{}, minImageCount {} (old swapchain {})",
        info.image_extent.width,
        info.image_extent.height,
        info.min_image_count,
        if info.old_swapchain == 0 { "none" } else { "recreated" },
    );
    let asked = info.present_mode;

    let PresentModeChoice::Prefer(preferences) = present_mode_choice() else {
        println!(
            "[android] vulkan: swapchain present mode {} (the engine's own choice; \
             CORDIAL_PRESENT_MODE=off)",
            present_mode_name(asked)
        );
        return f(device, create_info, allocator, swapchain_out);
    };

    let Some(supported) = supported_present_modes(info.surface) else {
        println!(
            "[android] vulkan: swapchain present mode {} (could not ask the driver what it \
             supports, so the engine's own choice stands)",
            present_mode_name(asked)
        );
        return f(device, create_info, allocator, swapchain_out);
    };
    let advertised: Vec<&str> = supported.iter().map(|&m| present_mode_name(m)).collect();

    let chosen = preferences.iter().copied().find(|m| supported.contains(m)).unwrap_or(asked);
    if chosen == asked {
        println!(
            "[android] vulkan: swapchain present mode {} (unchanged; driver offers {})",
            present_mode_name(asked),
            advertised.join(", ")
        );
        return f(device, create_info, allocator, swapchain_out);
    }

    println!(
        "[android] vulkan: swapchain present mode {} -> {} (driver offers {})",
        present_mode_name(asked),
        present_mode_name(chosen),
        advertised.join(", ")
    );
    // Substituting the mode alone leaves the swapchain sized for the mode the
    // engine asked for, and that is not the same swapchain. FIFO is a queue:
    // two images is enough, because the presentation engine hands one back
    // every refresh. MAILBOX and IMMEDIATE are not queues -- the pending image
    // is *replaced*, so with two images the only spare is the one on screen and
    // the renderer stalls in `vkAcquireNextImageKHR` waiting for a refresh it
    // was substituted in specifically to stop waiting for. Three is the count
    // that makes the substitution mean anything.
    //
    // Raised, never lowered, and clamped to what the driver will hold:
    // `maxImageCount == 0` is the specification's own "no limit" and not a
    // limit of zero.
    //
    // Measured rather than assumed, and the answer is that this has never
    // fired. On 2026-08-20 the engine asked for `minImageCount 3` on every
    // swapchain; **on 2026-08-25, on the same build of Cordial and a current
    // APK, every swapchain in the log asks for 4** -- first creation and
    // recreation alike. The engine's request is not a constant and the older
    // note read as though it were. The conclusion survives the correction,
    // because both numbers are already at least three, so the raise below stays
    // untaken; but nobody should quote "the engine asks for three" as a fact
    // about this engine. Kept because the substitution is what makes three a
    // *requirement* rather than the engine's preference, and a build that asked
    // for two would otherwise stall silently.
    let mut images = info.min_image_count;
    if !matches!(chosen, VK_PRESENT_MODE_FIFO_KHR | VK_PRESENT_MODE_FIFO_RELAXED_KHR)
        && images < 3
    {
        match surface_image_count_limits(info.surface) {
            Some((_, max)) if max != 0 && max < 3 => println!(
                "[android] vulkan: minImageCount stays {images}; {} wants three and the driver \
                 caps this surface at {max}",
                present_mode_name(chosen),
            ),
            Some(_) => {
                println!(
                    "[android] vulkan: minImageCount {images} -> 3 (a replacement mode needs a \
                     spare image; two would stall the renderer on the refresh {} exists to skip)",
                    present_mode_name(chosen),
                );
                images = 3;
            }
            None => println!(
                "[android] vulkan: minImageCount stays {images}; could not ask the driver how \
                 many images this surface allows"
            ),
        }
    }
    let patched =
        VkSwapchainCreateInfoKHR { present_mode: chosen, min_image_count: images, ..*info };
    // SAFETY: `patched` differs from the caller's struct in two scalar fields
    // and matches the host's layout exactly (see the module doc on Vulkan's ABI
    // being the same struct on Android and desktop Linux); every pointer inside
    // it is the caller's own and outlives this call.
    f(device, &patched, allocator, swapchain_out)
}

static HOST_GET_SURFACE_CAPS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// `0xFFFFFFFF` in both dimensions of `VkSurfaceCapabilitiesKHR::currentExtent`
/// is not a sentinel Cordial invented — it is `VK_KHR_wayland_surface`'s own
/// documented value for "the surface size is determined by the swapchain
/// being created", because unlike an X11 window or a real Android
/// `ANativeWindow`, a Wayland surface has no size of its own until a buffer
/// is attached to it.
const VK_WHOLE_SIZE_UNDEFINED_EXTENT: u32 = 0xFFFF_FFFF;

/// `vkGetPhysicalDeviceSurfaceCapabilitiesKHR`, patched on the Wayland backend
/// only.
///
/// This is the actual cause of the blank window, found by instrumenting the
/// Vulkan calls Roblox makes (not by reading FLog, which logs nothing helpful
/// here on either backend — a similarly-worded `Invalid currentExtent -1x-1`
/// line fires continuously on *both*, from an unrelated, harmless periodic
/// check, and very nearly passed for the cause before the real one was
/// measured). What actually differs, read straight from the values Mesa
/// returns to this same call:
///
/// ```text
///           currentExtent            calls to vkCreateSwapchainKHR that follow
/// X11       1280x720 (real)           1
/// Wayland   4294967295x4294967295     0
/// ```
///
/// `4294967295` is `0xFFFFFFFF` — the documented Wayland WSI value above, and
/// Roblox's own log confirms it reads that as invalid rather than as the
/// sentinel it is: `Vulkan: skipping framebuffer creation, invalid
/// currentExtent -1x-1`, repeated every frame, forever, because nothing ever
/// gives it a different answer. The engine's surface code was written against
/// Android's `ANativeWindow`-backed `VkSurfaceKHR`, which — like X11 — always
/// has a real, queryable size; it has no path for "you choose", so it never
/// reaches `vkCreateSwapchainKHR` at all.
///
/// The fix is the same substitution this whole file already makes for the
/// surface identity itself: report what an Android surface would report.
/// Cordial's own window is the one source of truth for "how big is Cordial's
/// window" everywhere else in this codebase (`ANativeWindow_getWidth`,
/// `wl_egl_window_resize` on the EGL path) — using it here too, instead of
/// Mesa's honestly-correct-per-spec-but-Android-shaped-code-hostile answer,
/// keeps that one source of truth rather than adding a second.
extern "C" fn vk_get_physical_device_surface_capabilities_khr(
    physical_device: *mut c_void,
    surface: *mut c_void,
    out: *mut VkSurfaceCapabilitiesKHR,
) -> i32 {
    let f = HOST_GET_SURFACE_CAPS.load(std::sync::atomic::Ordering::Relaxed);
    if f == 0 {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    type Fn_ = extern "C" fn(*mut c_void, *mut c_void, *mut VkSurfaceCapabilitiesKHR) -> i32;
    // SAFETY: resolved from the host loader for exactly this name.
    let f: Fn_ = unsafe { std::mem::transmute(f) };
    let rc = f(physical_device, surface, out);
    if rc != VK_SUCCESS || crate::android::backend() != crate::android::Backend::Wayland {
        return rc;
    }
    // SAFETY: `out` is the caller's own out-parameter and `rc == VK_SUCCESS`,
    // so Mesa has just written a complete `VkSurfaceCapabilitiesKHR` into it;
    // this file's definition of that struct matches Mesa's ABI (see the
    // module doc's general point about Vulkan's layout being identical on
    // Android and desktop Linux).
    let Some(caps) = (unsafe { out.as_mut() }) else {
        return rc;
    };
    if caps.current_extent.width == VK_WHOLE_SIZE_UNDEFINED_EXTENT
        && caps.current_extent.height == VK_WHOLE_SIZE_UNDEFINED_EXTENT
    {
        if let Some(w) = crate::android::wayland::current() {
            let (width, height, _) = w.geometry();
            // TEMPORARY INSTRUMENTATION -- not for commit. `CORDIAL_INSTR=1`.
            // This is the extent the swapchain is actually built from. If it
            // disagrees with the window after a fullscreen toggle, the content
            // is drawn against the wrong rectangle and offsets by the
            // difference — which is what issue #7 looks like on screen.
            if std::env::var_os("CORDIAL_INSTR").is_some() {
                eprintln!("[instr] surface_caps currentExtent <- geometry() {width}x{height}");
            }
            // Clamped into [minImageExtent, maxImageExtent] on principle —
            // Cordial's window is always within Mesa's advertised range in
            // practice (1x1..16384x16384 observed), but a substitution that
            // could itself hand back an out-of-range extent would just move
            // this bug rather than fix it.
            let clamp = |v: i32, lo: u32, hi: u32| (v.max(0) as u32).clamp(lo, hi);
            caps.current_extent.width =
                clamp(width, caps.min_image_extent.width, caps.max_image_extent.width);
            caps.current_extent.height =
                clamp(height, caps.min_image_extent.height, caps.max_image_extent.height);
            crate::android::trace(format_args!(
                "wayland: vkGetPhysicalDeviceSurfaceCapabilitiesKHR currentExtent was undefined \
                 (0xFFFFFFFF), reporting the window's own {}x{}",
                caps.current_extent.width, caps.current_extent.height,
            ));
            // And once per *change*, unconditionally. The engine asks this
            // several times a second, so a per-call line is unreadable and a
            // trace flag hides the one thing a fullscreen report needs: the
            // size Cordial claimed the surface was, at the moment it claimed
            // it. On Wayland this substitution is the only source of that
            // number — Mesa never supplies one — so if it never changes across
            // a fullscreen transition, no swapchain the engine builds
            // afterwards can be the right size.
            let packed = ((caps.current_extent.width as u64) << 32)
                | caps.current_extent.height as u64;
            static LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            if LAST.swap(packed, std::sync::atomic::Ordering::Relaxed) != packed {
                println!(
                    "[android] vulkan: reporting surface extent {}x{} to the engine",
                    caps.current_extent.width, caps.current_extent.height,
                );
            }
        }
    }
    rc
}

extern "C" fn vk_create_instance(
    create_info: *const VkInstanceCreateInfo,
    allocator: *const c_void,
    instance_out: *mut *mut c_void,
) -> i32 {
    let Some(h) = host() else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let Some(info) = (unsafe { create_info.as_ref() }) else {
        // SAFETY: `create_info` is caller-supplied; a null pointer here is the
        // caller's bug, not this shim's — hand it to the host unchanged and let
        // it report `VK_ERROR_INITIALIZATION_FAILED` on its own terms.
        return unsafe { (h.create_instance)(create_info, allocator, instance_out) };
    };

    let count = info.enabled_extension_count as usize;
    // SAFETY: `count` and `pp_enabled_extension_names` are the caller's own
    // paired length and pointer, per the Vulkan struct contract.
    let names: &[*const c_char] = if count == 0 || info.pp_enabled_extension_names.is_null() {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(info.pp_enabled_extension_names, count) }
    };

    // The host must never be asked to enable an extension it does not
    // implement. `VK_KHR_android_surface` only exists in what Roblox sees —
    // `vk_enumerate_instance_extension_properties` invented it — so it is
    // rewritten back to the real extension for whichever backend is live
    // before this ever reaches Mesa. Everything else passes through
    // untouched, including extensions this shim knows nothing about; the
    // host rejecting one it truly lacks is the correct failure, not
    // something to mask here.
    let real_name = real_surface_extension_name();
    let mut swapped = false;
    let rewritten: Vec<*const c_char> = names
        .iter()
        .map(|&p| {
            if !p.is_null() && unsafe { CStr::from_ptr(p) }.to_bytes() == b"VK_KHR_android_surface"
            {
                swapped = true;
                real_name.as_ptr()
            } else {
                p
            }
        })
        .collect();

    crate::android::trace(format_args!(
        "vkCreateInstance: {count} extension(s) requested, VK_KHR_android_surface -> {}: {swapped}",
        real_name.to_string_lossy()
    ));

    let patched = VkInstanceCreateInfo {
        pp_enabled_extension_names: if rewritten.is_empty() {
            info.pp_enabled_extension_names
        } else {
            rewritten.as_ptr()
        },
        ..*info
    };
    // SAFETY: `patched` matches the host's `VkInstanceCreateInfo` layout exactly
    // (see the module doc); `rewritten` outlives this call.
    let rc = unsafe { (h.create_instance)(&patched, allocator, instance_out) };

    // Kept because instance-level commands cannot be reached without it:
    // `vkGetInstanceProcAddr(NULL, ...)` only answers for the three global
    // commands, and `supported_present_modes` needs one that is not among them.
    if rc == VK_SUCCESS {
        // SAFETY: `vkCreateInstance` returning `VK_SUCCESS` is its own contract
        // that it wrote a valid handle through `instance_out`.
        if let Some(&instance) = unsafe { instance_out.as_ref() } {
            HOST_INSTANCE.store(instance as usize, std::sync::atomic::Ordering::Relaxed);
        }
    }
    rc
}

extern "C" fn vk_enumerate_instance_extension_properties(
    layer_name: *const c_char,
    property_count: *mut u32,
    properties: *mut VkExtensionProperties,
) -> i32 {
    let Some(h) = host() else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if property_count.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }

    // The two-call idiom, run once against the host to build the real list.
    let mut host_count: u32 = 0;
    // SAFETY: `layer_name` is the caller's own pointer, forwarded unchanged;
    // `host_count` is a valid local out-parameter.
    let rc = unsafe {
        (h.enumerate_instance_extension_properties)(layer_name, &mut host_count, std::ptr::null_mut())
    };
    if rc != VK_SUCCESS {
        return rc;
    }
    let mut combined = vec![VkExtensionProperties::zeroed(); host_count as usize];
    if host_count > 0 {
        // SAFETY: `combined` has exactly `host_count` elements, matching the
        // count this same call just reported.
        let rc = unsafe {
            (h.enumerate_instance_extension_properties)(
                layer_name,
                &mut host_count,
                combined.as_mut_ptr(),
            )
        };
        if rc != VK_SUCCESS {
            return rc;
        }
        combined.truncate(host_count as usize);
    }

    // Mesa reports its own name for this capability — `VK_KHR_xlib_surface`
    // on X11, `VK_KHR_wayland_surface` on Wayland, per whichever backend
    // `real_surface_extension_name` says is live. Roblox will only ever ask a
    // Vulkan loader for `VK_KHR_android_surface`, because that is the only
    // surface extension Android ever had. Advertise it whenever the host has
    // the capability it stands in for — layer-provided extension lists
    // (`layer_name` non-null) are left as the layer reported them, since this
    // is an ICD-level substitution, not a layer's.
    let real_name = real_surface_extension_name();
    let has_real = combined.iter().any(|p| p.name_matches(real_name.to_bytes()));
    let has_android = combined
        .iter()
        .any(|p| p.name_matches(b"VK_KHR_android_surface"));
    if layer_name.is_null() && has_real && !has_android {
        combined.push(VkExtensionProperties::named(
            "VK_KHR_android_surface",
            ANDROID_SURFACE_SPEC_VERSION,
        ));
        crate::android::trace(format_args!(
            "vkEnumerateInstanceExtensionProperties: advertising VK_KHR_android_surface (backing {})",
            real_name.to_string_lossy()
        ));
    }

    let combined_count = combined.len() as u32;
    if properties.is_null() {
        // SAFETY: caller-supplied out-parameter, per the Vulkan two-call idiom.
        unsafe { *property_count = combined_count };
        return VK_SUCCESS;
    }

    // SAFETY: the caller sets `*property_count` to the capacity of `properties`
    // before this call, per the Vulkan two-call idiom.
    let requested = unsafe { *property_count };
    let to_copy = requested.min(combined_count);
    // SAFETY: `properties` has room for at least `requested` entries per the
    // caller's own contract; `to_copy` is bounded by both that and `combined`'s
    // real length.
    unsafe {
        std::ptr::copy_nonoverlapping(combined.as_ptr(), properties, to_copy as usize);
        *property_count = to_copy;
    }
    if to_copy < combined_count {
        VK_INCOMPLETE
    } else {
        VK_SUCCESS
    }
}

/// `vkCreateAndroidSurfaceKHR`, answered with `vkCreateXlibSurfaceKHR` on X11
/// or `vkCreateWaylandSurfaceKHR` on Wayland, according to
/// [`crate::android::backend`].
///
/// The `ANativeWindow*` inside `pCreateInfo` is Cordial's own — there is
/// exactly one window — so it is not read; the real handles come from
/// whichever backend's window singleton is live, the same pair
/// `egl_create_window_surface` already substitutes for EGL in each backend's
/// own module (`window.rs`/`wayland.rs`).
extern "C" fn vk_create_android_surface_khr(
    instance: *mut c_void,
    create_info: *const VkAndroidSurfaceCreateInfoKHR,
    allocator: *const c_void,
    surface_out: *mut *mut c_void,
) -> i32 {
    let _ = create_info;
    let Some(h) = host() else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };

    match crate::android::backend() {
        crate::android::Backend::Wayland => {
            let Some(win) = crate::android::wayland::current() else {
                return VK_ERROR_INITIALIZATION_FAILED;
            };
            // SAFETY: `instance` is a real host `VkInstance` by the time the
            // engine can reach this call, and the name is Mesa's own
            // documented export.
            let f = unsafe {
                (h.get_instance_proc_addr)(instance, c"vkCreateWaylandSurfaceKHR".as_ptr())
            };
            if f.is_null() {
                return VK_ERROR_EXTENSION_NOT_PRESENT;
            }
            type Fn_ = unsafe extern "C" fn(
                *mut c_void,
                *const VkWaylandSurfaceCreateInfoKHR,
                *const c_void,
                *mut *mut c_void,
            ) -> i32;
            // SAFETY: resolved from the host for exactly this name.
            let f: Fn_ = unsafe { std::mem::transmute(f) };
            let wayland_info = VkWaylandSurfaceCreateInfoKHR {
                s_type: VK_STRUCTURE_TYPE_WAYLAND_SURFACE_CREATE_INFO_KHR,
                p_next: std::ptr::null(),
                flags: 0,
                display: win.wl_display(),
                surface: win.wl_surface(),
            };
            crate::android::trace(format_args!(
                "vkCreateAndroidSurfaceKHR -> vkCreateWaylandSurfaceKHR"
            ));
            // SAFETY: `wayland_info` matches Mesa's
            // `VkWaylandSurfaceCreateInfoKHR` layout exactly (see the module
            // doc); `instance`, `allocator` and `surface_out` are the
            // caller's own arguments, forwarded unchanged.
            unsafe { f(instance, &wayland_info, allocator, surface_out) }
        }
        crate::android::Backend::X11 => {
            let Some(win) = crate::android::window::current() else {
                return VK_ERROR_INITIALIZATION_FAILED;
            };
            // SAFETY: as above.
            let f =
                unsafe { (h.get_instance_proc_addr)(instance, c"vkCreateXlibSurfaceKHR".as_ptr()) };
            if f.is_null() {
                return VK_ERROR_EXTENSION_NOT_PRESENT;
            }
            type Fn_ = unsafe extern "C" fn(
                *mut c_void,
                *const VkXlibSurfaceCreateInfoKHR,
                *const c_void,
                *mut *mut c_void,
            ) -> i32;
            // SAFETY: resolved from the host for exactly this name.
            let f: Fn_ = unsafe { std::mem::transmute(f) };
            let xlib_info = VkXlibSurfaceCreateInfoKHR {
                s_type: VK_STRUCTURE_TYPE_XLIB_SURFACE_CREATE_INFO_KHR,
                p_next: std::ptr::null(),
                flags: 0,
                dpy: win.egl_native_display(),
                window: win.egl_native_window(),
            };
            crate::android::trace(format_args!("vkCreateAndroidSurfaceKHR -> vkCreateXlibSurfaceKHR"));
            // SAFETY: `xlib_info` matches Mesa's `VkXlibSurfaceCreateInfoKHR`
            // layout exactly (see the module doc); `instance`, `allocator`
            // and `surface_out` are the caller's own arguments, forwarded
            // unchanged.
            unsafe { f(instance, &xlib_info, allocator, surface_out) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What nothing-set resolves to. Named rather than spelled out at each
    /// use, because the whole point of these tests is that the fallback is one
    /// decision made in one place.
    const DEFAULT: PresentModeChoice = PresentModeChoice::Prefer(&[VK_PRESENT_MODE_MAILBOX_KHR]);

    fn resolved(env: Option<&str>, flag: Option<(&str, &str)>) -> PresentModeChoice {
        resolve_present_mode(
            env.map(str::to_string),
            flag.map(|(s, v)| (s.to_string(), v.to_string())),
        )
        .0
    }

    /// **The default is MAILBOX, and it is latency that decides it.**
    ///
    /// This assertion has now been written three ways and the history is the
    /// useful part. It first said MAILBOX and called it "the behaviour Cordial
    /// has always had", which was true of the code and false of the machine --
    /// before this file had any present-mode handling the engine picked FIFO
    /// for itself, so MAILBOX was a change Cordial introduced. It was then
    /// flipped to FIFO on a power argument, reasoned and not measured. A user
    /// measured it inside the hour: "the mouse feels floaty and weird", and
    /// then the control, "switching back to Mailbox fixes the floaty fealing".
    ///
    /// So MAILBOX, because FIFO queues presents against the display clock and
    /// the cursor lags the hand by the queue depth. The power cost of MAILBOX
    /// is real and unmeasured here; `fifo` is one row away in Settings for
    /// anybody who would rather pay latency than battery.
    #[test]
    fn nothing_set_is_mailbox_because_fifo_measured_floaty() {
        assert_eq!(resolved(None, None), DEFAULT);
        assert_eq!(resolved(Some(""), None), DEFAULT);
        assert_eq!(
            resolved(None, None),
            PresentModeChoice::Prefer(&[VK_PRESENT_MODE_MAILBOX_KHR]),
            "spelled out once, so a change to DEFAULT cannot make this test agree with itself"
        );
    }

    /// Every mode the settings row offers must be reachable by name, or the
    /// row is decoration. `fifo` especially: it is no longer the default, so
    /// nothing else in this file would notice if it stopped resolving.
    #[test]
    fn every_mode_the_settings_row_offers_is_reachable_by_name() {
        // `Prefer` holds a `&'static [i32]`, so the expected values are
        // statics rather than a temporary built in the loop.
        const FIFO: &[i32] = &[VK_PRESENT_MODE_FIFO_KHR];
        const MAILBOX: &[i32] = &[VK_PRESENT_MODE_MAILBOX_KHR];
        const IMMEDIATE: &[i32] = &[VK_PRESENT_MODE_IMMEDIATE_KHR];
        for (name, want) in [("fifo", FIFO), ("mailbox", MAILBOX), ("immediate", IMMEDIATE)] {
            assert_eq!(resolved(Some(name), None), PresentModeChoice::Prefer(want), "{name}");
        }
    }

    #[test]
    fn uncapped_takes_the_untearing_one_first_and_falls_back_to_the_tearing_one() {
        // A plugin asking for an uncapped frame rate should not have to know
        // which modes this particular surface advertises. MAILBOX uncaps
        // without tearing; IMMEDIATE always exists.
        assert_eq!(
            resolved(Some("uncapped"), None),
            PresentModeChoice::Prefer(&[
                VK_PRESENT_MODE_MAILBOX_KHR,
                VK_PRESENT_MODE_IMMEDIATE_KHR
            ])
        );
    }

    #[test]
    fn a_plugin_can_ask_for_a_present_mode_without_being_given_the_swapchain() {
        // ADR-007 and ADR-020: the plugin contributes a value to a flag layer
        // and Cordial performs the effect. This is the whole of the mechanism
        // on the runtime's side.
        assert_eq!(
            resolved(None, Some(("plugin:fps-flex", "immediate"))),
            PresentModeChoice::Prefer(&[VK_PRESENT_MODE_IMMEDIATE_KHR])
        );
    }

    #[test]
    fn an_explicit_setting_beats_a_plugin_and_auto_does_not() {
        // The precedence `graphics.rs` established. Somebody who set the
        // variable — a measurement run, or the shell — must be able to
        // overrule a plugin they installed to do something else; somebody who
        // said "auto" said "you decide", not "ignore everything".
        assert_eq!(resolved(Some("fifo"), Some(("plugin:p", "immediate"))), 
                   PresentModeChoice::Prefer(&[VK_PRESENT_MODE_FIFO_KHR]));
        assert_eq!(
            resolved(Some("auto"), Some(("plugin:p", "immediate"))),
            PresentModeChoice::Prefer(&[VK_PRESENT_MODE_IMMEDIATE_KHR])
        );
    }

    #[test]
    fn the_control_for_a_measurement_is_still_reachable() {
        // `off` forwards whatever the engine asked for, which is the arm every
        // present-mode timing claim here needs beside it.
        assert_eq!(resolved(Some("off"), None), PresentModeChoice::Untouched);
        assert_eq!(resolved(Some("engine"), None), PresentModeChoice::Untouched);
    }

    #[test]
    fn a_value_nobody_understands_falls_back_rather_than_guessing() {
        // From either source. A misspelled mode must not silently become the
        // one whose name it is closest to.
        assert_eq!(resolved(Some("imediate"), None), DEFAULT);
        assert_eq!(resolved(None, Some(("plugin:p", "as-fast-as-possible"))), DEFAULT);
    }

    #[test]
    fn the_present_mode_key_is_cordials_own_and_never_reaches_roblox() {
        // `client_settings::is_roblox_flag` filters on the `Cordial` prefix,
        // so this name must keep it. A key that lost the prefix would be sent
        // to the engine as though it were a FastFlag.
        assert!(PRESENT_MODE_KEY.starts_with("Cordial"));
    }
}
