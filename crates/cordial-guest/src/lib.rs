//! Runs arm64 guest code inside an x86-64 Cordial, through dynarmic.
//!
//! This is Phase 2 of the VR fork, designed in `docs/vr/dynarmic-design.md`:
//! the Quest build's `libroblox.so` is arm64, and rather than run the whole
//! client under qemu-user the engine's own instructions are translated in
//! process while everything it imports stays native. What lives here is the
//! mechanism only -- the Jit per thread, the stub page the guest calls the
//! host through, the AAPCS64-to-SysV call builder, the host-to-guest call,
//! host entry points that let the host call a guest function as its own
//! (`entry.rs`), the AAPCS64 `va_list` reader, the guest's `mmap` family with
//! the translations a change to its code invalidates (`code.rs`), and the few
//! thunks that cannot be generic (variadic, callback-taking). Nothing here
//! knows about Roblox.
//!
//! The line ADR-001 draws is kept by construction (design §7): dispatch is
//! keyed only on SVC immediates in Cordial's own stub page, never on a guest
//! address, and nothing here writes guest code.
//!
//! On any target other than x86-64 the crate is empty: dynarmic's A64
//! frontend has no arm64 host backend, and the aarch64 build runs the engine
//! natively (or under qemu-user) instead. It is empty without the `dynarmic`
//! feature too, which `cordial-runtime/vr` turns on: dynarmic needs Boost's
//! headers, and a checkout's plain `cargo build` should not.

// One of the ABI edges ADR-036 names: every guest register is a raw pointer
// or a raw bit pattern the host has to reinterpret, and the Jit itself is a
// C++ object behind a C shim. Denying `unsafe_code` would hide that, not
// remove it.
#![allow(unsafe_code)]

#[cfg(all(feature = "dynarmic", target_arch = "x86_64"))]
mod abi;
#[cfg(all(feature = "dynarmic", target_arch = "x86_64"))]
mod clock;
#[cfg(all(feature = "dynarmic", target_arch = "x86_64"))]
pub mod code;
#[cfg(all(feature = "dynarmic", target_arch = "x86_64"))]
mod entry;
#[cfg(all(feature = "dynarmic", target_arch = "x86_64"))]
mod ffi;
#[cfg(all(feature = "dynarmic", target_arch = "x86_64"))]
mod jit;
#[cfg(all(feature = "dynarmic", target_arch = "x86_64"))]
mod keys;
#[cfg(all(feature = "dynarmic", target_arch = "x86_64"))]
mod mem;
#[cfg(all(feature = "dynarmic", target_arch = "x86_64"))]
mod string;
#[cfg(all(feature = "dynarmic", target_arch = "x86_64"))]
pub mod thunks;

#[cfg(all(feature = "dynarmic", target_arch = "x86_64"))]
pub use abi::{collect, invoke, printf_types, write_ret, Ret, Ty, VaList};
#[cfg(all(feature = "dynarmic", target_arch = "x86_64"))]
pub use entry::{host_entry, host_entry_count, ArgMap};
#[cfg(all(feature = "dynarmic", target_arch = "x86_64"))]
pub use ffi::HostRet;
#[cfg(all(feature = "dynarmic", target_arch = "x86_64"))]
pub use keys::{key_clean_all, KEYS};
#[cfg(all(feature = "dynarmic", target_arch = "x86_64"))]
pub use string::STRING_FUNCTIONS;

/// Guest instructions the translator has fetched to translate, process-wide
/// (a block translated twice counts twice).
#[cfg(all(feature = "dynarmic", target_arch = "x86_64"))]
pub fn translated_instructions() -> u64 {
    // SAFETY: reads an atomic counter.
    unsafe { ffi::cg_translated_instructions() }
}

/// `strtold`, returning the guest's binary128 `long double` as two words.
/// See `cg_strtold_quad` in `native/shim.cpp` for what is and is not exact.
///
/// # Safety
///
/// `s` must be a C string and `end` null or writable.
#[cfg(all(feature = "dynarmic", target_arch = "x86_64"))]
pub unsafe fn strtold_quad(s: *const std::ffi::c_char, end: *mut *mut std::ffi::c_char, out: &mut [u64; 2]) {
    // SAFETY: the caller's guarantee.
    unsafe { ffi::cg_strtold_quad(s, end, out) }
}
#[cfg(all(feature = "dynarmic", target_arch = "x86_64"))]
pub use jit::{
    guest_call, guest_call_args, guest_stack_of, set_trace, guest_threads, last_fault_context, set_thread_stack,
    thread_depth, thread_guest_stack, thread_jit_count, Call, Exception, Fault, FaultContext, Handler, MonitorMode, Options,
    Returned, Runtime, StackSpec, Stats, ThreadInfo, UNSAFE_FP_INACCURATE_NAN, UNSAFE_FP_REDUCED_ERROR,
    UNSAFE_FP_UNFUSE_FMA,
};
#[cfg(all(feature = "dynarmic", target_arch = "x86_64"))]
pub use mem::Mapping;
