//! The C ABI of `native/shim.cpp` and `native/host_call_x86_64.S`.

use std::ffi::c_void;

#[repr(C)]
pub struct CgJit {
    _opaque: [u8; 0],
}

#[repr(C)]
pub struct CgCallbacks {
    pub user: *mut c_void,
    pub svc: extern "C" fn(user: *mut c_void, jit: *mut CgJit, imm: u32),
    pub exception: extern "C" fn(user: *mut c_void, jit: *mut CgJit, pc: u64, kind: u32),
    pub fallback: extern "C" fn(user: *mut c_void, jit: *mut CgJit, pc: u64, count: u64),
    pub icache: extern "C" fn(user: *mut c_void, jit: *mut CgJit, op: u32, va: u64),
}

#[repr(C)]
pub struct CgConfig {
    pub callbacks: CgCallbacks,
    pub tpidr_el0: *mut u64,
    pub tpidrro_el0: *const u64,
    pub monitor: *mut c_void,
    pub processor_id: u64,
    pub fastmem_exclusive: u8,
    pub ignore_global_monitor: u8,
    pub code_cache_size: u64,
    pub unsafe_fp: u32,
}

/// What `cg_host_call` captured from the SysV return registers.
#[repr(C)]
#[derive(Default)]
pub struct HostRet {
    pub rax: u64,
    pub rdx: u64,
    pub xmm0: [u64; 2],
    pub xmm1: [u64; 2],
}

unsafe extern "C" {
    pub fn cg_monitor_new(processors: u64) -> *mut c_void;
    pub fn cg_monitor_free(m: *mut c_void);

    pub fn cg_jit_new(c: *const CgConfig) -> *mut CgJit;
    pub fn cg_translated_instructions() -> u64;
    pub fn cg_jit_free(j: *mut CgJit);
    pub fn cg_jit_run(j: *mut CgJit) -> u32;
    pub fn cg_jit_halt(j: *mut CgJit, reason: u32);
    pub fn cg_jit_invalidate(j: *mut CgJit, start: u64, len: u64);
    pub fn cg_jit_get_x(j: *const CgJit, i: u32) -> u64;
    pub fn cg_jit_set_x(j: *mut CgJit, i: u32, v: u64);
    pub fn cg_jit_get_sp(j: *const CgJit) -> u64;
    pub fn cg_jit_set_sp(j: *mut CgJit, v: u64);
    pub fn cg_jit_get_pc(j: *const CgJit) -> u64;
    pub fn cg_jit_set_pc(j: *mut CgJit, v: u64);
    pub fn cg_jit_get_v(j: *const CgJit, i: u32, out: *mut [u64; 2]);
    pub fn cg_jit_set_v(j: *mut CgJit, i: u32, v: *const [u64; 2]);

    pub fn cg_strtold_quad(s: *const std::ffi::c_char, end: *mut *mut std::ffi::c_char, out: *mut [u64; 2]);

    pub fn cg_host_call_guarded(
        f: *const c_void,
        gpr: *const [u64; 6],
        xmm: *const [[u64; 2]; 8],
        stack: *const u64,
        nstack: u64,
        out: *mut HostRet,
        err: *mut std::ffi::c_char,
        err_len: usize,
    ) -> i32;
}

/// dynarmic's `HaltReason` bits this crate uses. `UserDefined1..3`.
pub const HALT_RETURNED: u32 = 0x0100_0000;
/// dynarmic's own `CacheInvalidation`, which `InvalidateCacheRange` sets.
pub const HALT_CACHE_INVALIDATION: u32 = 0x0000_0002;
pub const HALT_FAULT: u32 = 0x0200_0000;
