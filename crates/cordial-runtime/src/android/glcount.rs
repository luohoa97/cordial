//! Counts the graphics calls Roblox makes.
//!
//! "The engine took the surface" and "the engine is drawing" are different
//! claims. Nothing in the log distinguishes them, and a window can look plausible
//! while the compositor is painting all of it. Counting the calls that only a
//! rendering engine makes — creating a window surface, clearing, drawing,
//! swapping — settles it either way.
//!
//! These wrap the host's real functions and forward, so they cost a counter
//! increment and one relaxed atomic load per call and change nothing else.
//!
//! **That sentence used to say "a counter increment per call" and was wrong**,
//! from the day it was written until 2026-08-26. The forwarding macro resolved
//! the host function *inside* the wrapper body, so every wrapped call also
//! allocated a `CString`, took glibc's global linker lock, walked the ELF link
//! map and freed the string again. `glDrawElements` is wrapped. Counting is
//! gated behind `CORDIAL_COUNT_GL`, so no shipped client ever paid it — but
//! **anything timed with the counters on was timing the allocator and the
//! loader too**, which makes this the exact failure this project keeps writing
//! rules about: an instrument that changes what it measures, next to a comment
//! saying it does not.

use std::ffi::{c_void, CString};
use std::sync::atomic::{AtomicU64, Ordering};

macro_rules! counters {
    ($($name:ident => $sym:literal),* $(,)?) => {
        $(pub static $name: AtomicU64 = AtomicU64::new(0);)*

        /// Every counter, for reporting.
        pub fn report() -> Vec<(&'static str, u64)> {
            vec![$(($sym, $name.load(Ordering::Relaxed))),*]
        }
    };
}

counters! {
    CREATE_WINDOW_SURFACE => "eglCreateWindowSurface",
    MAKE_CURRENT          => "eglMakeCurrent",
    SWAP_BUFFERS          => "eglSwapBuffers",
    CLEAR                 => "glClear",
    DRAW_ELEMENTS         => "glDrawElements",
    DRAW_ARRAYS           => "glDrawArrays",
    COMPILE_SHADER        => "glCompileShader",
    TEX_IMAGE_2D          => "glTexImage2D",
    // Vulkan's counterpart to eglSwapBuffers. Without it the report reads all
    // zeros on a Vulkan session and looks exactly like "nothing rendered".
    QUEUE_PRESENT         => "vkQueuePresentKHR",
    // The T3/NVIDIA-textures question from TASKS.md ("detex: premise
    // unproven") and docs/adr/ADR-042: whether the engine ever asks the host
    // driver about a mobile compressed format at all, and whether any such
    // query comes back with zero support. Incremented from
    // `vulkan::vk_get_physical_device_format_properties`, never from a read of
    // the binary or a guess.
    FORMAT_QUERY_ETC2        => "vkGetPhysicalDeviceFormatProperties(ETC2)",
    FORMAT_QUERY_ASTC        => "vkGetPhysicalDeviceFormatProperties(ASTC)",
    FORMAT_QUERY_BC          => "vkGetPhysicalDeviceFormatProperties(BC)",
    FORMAT_QUERY_OTHER       => "vkGetPhysicalDeviceFormatProperties(other)",
    FORMAT_QUERY_UNSUPPORTED => "  ...of which unsupported (both tiling feature masks zero)",
    CREATE_SHADER_MODULE     => "vkCreateShaderModule",
    ETC_IMAGE_EMULATED       => "vkCreateImage(ETC2/EAC, emulated)",
    ETC_REGION_DECODED       => "vkCmdCopyBufferToImage(ETC2/EAC regions decoded)",
    ETC_LATE_WRITE           => "  ...re-decoded at submit (source changed after recording)",
    ETC_UNHANDLED            => "ETC2/EAC emulation: unhandled calls (see trace)",
}

/// The host implementation behind `sym`, looked up.
///
/// Called once per wrapped symbol now rather than once per call; see
/// [`forward!`] for the cache and the module doc for what it cost before.
fn host(sym: &str) -> *mut c_void {
    unsafe extern "C" {
        fn dlsym(handle: *mut c_void, symbol: *const std::ffi::c_char) -> *mut c_void;
    }
    let Ok(c) = CString::new(sym) else {
        return std::ptr::null_mut();
    };
    // SAFETY: RTLD_DEFAULT with a valid symbol name; libEGL and libGLESv2 are
    // already loaded into the global scope by the symbol table.
    unsafe { dlsym(std::ptr::null_mut(), c.as_ptr()) }
}

/// Build a counting wrapper for `sym`, or `None` if the host lacks it.
///
/// The wrappers take and return `*mut c_void` uniformly and forward with the
/// caller's registers untouched, which works because every counted entry point
/// passes its arguments in integer registers. `glClearColor` takes floats and is
/// deliberately not counted for that reason.
macro_rules! forward {
    ($counter:ident, $sym:literal, ($($a:ident),*)) => {{
        unsafe extern "C" fn wrapper($($a: *mut c_void),*) -> *mut c_void {
            $counter.fetch_add(1, Ordering::Relaxed);
            type Fn_ = extern "C" fn($(replace!($a)),*) -> *mut c_void;
            // **Resolved once per symbol, not once per call.** Each expansion
            // of this macro gets its own static, so the cache is per wrapped
            // function with no map and no lock.
            //
            // Relaxed is enough and the race is benign: two threads that arrive
            // together both resolve the same name to the same address and both
            // store it. There is no state to publish alongside the pointer, so
            // there is nothing for an acquire to order against.
            static CACHED: std::sync::atomic::AtomicPtr<c_void> =
                std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
            let mut target = CACHED.load(Ordering::Relaxed);
            if target.is_null() {
                target = host($sym);
                CACHED.store(target, Ordering::Relaxed);
            }
            // SAFETY: resolved from the host for exactly this name, and called
            // with the arguments the caller passed through unchanged.
            let f: Fn_ = unsafe { std::mem::transmute(target) };
            f($($a),*)
        }
        ($sym, wrapper as *const () as *mut c_void)
    }};
}
macro_rules! replace {
    ($a:ident) => { *mut c_void };
}

// CORDIAL_SWAP_TIMES=1 prints a wall-clock timestamp and how long the real
// eglSwapBuffers blocked, around every call. This is what found the ~1fps
// GLES bug: it showed every swap blocking for 0.97-1.00s inside the host
// call itself (see the comment on `egl_swap_interval` in window.rs), settling
// "swaps are evenly spaced but each one blocks too long" against "swaps come
// in a burst then the engine stalls" without needing lldb, which does not
// break inside libroblox.so. Kept as a standing diagnostic — cheap when off,
// and the next swap-pacing regression on some other host will want it again.
// Not wired to any counter; purely stderr timing.
static SWAP_TRACE_T0: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

unsafe extern "C" fn swap_buffers_timed(a: *mut c_void, b: *mut c_void) -> *mut c_void {
    SWAP_BUFFERS.fetch_add(1, Ordering::Relaxed);
    let t0 = *SWAP_TRACE_T0.get_or_init(std::time::Instant::now);
    let before = t0.elapsed().as_secs_f64();
    unsafe extern "C" {
        fn dlsym(handle: *mut c_void, symbol: *const std::ffi::c_char) -> *mut c_void;
    }
    let f: extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void = unsafe {
        std::mem::transmute(dlsym(std::ptr::null_mut(), c"eglSwapBuffers".as_ptr()))
    };
    let r = f(a, b);
    let after = t0.elapsed().as_secs_f64();
    eprintln!("[swap-trace] enter={before:.4}s return={after:.4}s blocked={:.4}s", after - before);
    r
}

pub fn overrides() -> Vec<(&'static str, *mut c_void)> {
    let mut v = vec![
        forward!(MAKE_CURRENT, "eglMakeCurrent", (a, b, c, d)),
        forward!(CLEAR, "glClear", (a)),
        forward!(DRAW_ELEMENTS, "glDrawElements", (a, b, c, d)),
        forward!(DRAW_ARRAYS, "glDrawArrays", (a, b, c)),
        forward!(COMPILE_SHADER, "glCompileShader", (a)),
        forward!(TEX_IMAGE_2D, "glTexImage2D", (a, b, c, d, e, f, g, h, i)),
    ];
    if std::env::var_os("CORDIAL_SWAP_TIMES").is_some() {
        v.push(("eglSwapBuffers", swap_buffers_timed as *const () as *mut c_void));
    } else {
        v.push(forward!(SWAP_BUFFERS, "eglSwapBuffers", (a, b)));
    }
    v
}
