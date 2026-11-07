//! Anonymous mappings: guest stacks, the stub page, test images.

use std::ffi::c_void;

unsafe extern "C" {
    fn mmap(addr: *mut c_void, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut c_void;
    fn munmap(addr: *mut c_void, len: usize) -> i32;
    fn mprotect(addr: *mut c_void, len: usize, prot: i32) -> i32;
}

pub const PROT_READ: i32 = 1;
pub const PROT_WRITE: i32 = 2;
const MAP_PRIVATE: i32 = 0x02;
const MAP_ANONYMOUS: i32 = 0x20;
const MAP_NORESERVE: i32 = 0x4000;

/// An owned anonymous mapping, unmapped on drop.
///
/// Never mapped executable. The guest's code is only ever *read* -- dynarmic
/// fetches instructions through `MemoryReadCode` and runs its own
/// translation -- so a guest image, and in particular Cordial's stub page,
/// has no reason to be executable by the host, and not being so means a
/// stray host jump into one faults instead of running arm64 bytes as x86.
pub struct Mapping {
    ptr: *mut u8,
    len: usize,
    /// Inaccessible bytes below `ptr` that belong to the mapping too.
    guard: usize,
}

// SAFETY: a Mapping is plain memory with no thread affinity; who may write
// it when is the owner's concern, exactly as for a Vec's buffer.
unsafe impl Send for Mapping {}
// SAFETY: as above.
unsafe impl Sync for Mapping {}

impl Mapping {
    /// Zeroed, readable and writable, committed lazily by the kernel.
    pub fn new(len: usize) -> Mapping {
        let len = (len + 0xfff) & !0xfff;
        // SAFETY: a fresh anonymous mapping aliases nothing.
        let ptr = unsafe {
            mmap(std::ptr::null_mut(), len, PROT_READ | PROT_WRITE,
                 MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0)
        };
        assert!(ptr as isize != -1, "mmap of {len} bytes failed");
        Mapping { ptr: ptr.cast(), len, guard: 0 }
    }

    /// A guest thread's stack: `len` usable bytes above a 64 KiB guard that
    /// is neither readable nor writable, as bionic places one below each
    /// thread's stack. A guest overflow then faults at once instead of
    /// writing into whatever the kernel mapped below. `addr()` and `len()`
    /// describe the usable part.
    pub fn stack(len: usize) -> Mapping {
        const GUARD: usize = 64 << 10;
        let len = (len + 0xfff) & !0xfff;
        let whole = Mapping::new(len + GUARD);
        // SAFETY: the first GUARD bytes are this mapping's own.
        let r = unsafe { mprotect(whole.ptr.cast(), GUARD, 0) };
        assert_eq!(r, 0, "mprotect of a guard page failed");
        let ptr = whole.ptr;
        std::mem::forget(whole);
        // SAFETY: GUARD < the mapping's length.
        Mapping { ptr: unsafe { ptr.add(GUARD) }, len, guard: GUARD }
    }

    /// A mapping holding `bytes` at its start, followed by `extra` zero bytes
    /// (a flat image's `.bss` is not in the file).
    pub fn with_contents(bytes: &[u8], extra: usize) -> Mapping {
        let m = Mapping::new(bytes.len() + extra);
        // SAFETY: the mapping is at least bytes.len() long and freshly ours.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), m.ptr, bytes.len()) };
        m
    }

    pub fn addr(&self) -> u64 {
        self.ptr as u64
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn protect(&self, prot: i32) {
        // SAFETY: the whole range is this mapping's own.
        let r = unsafe { mprotect(self.ptr.cast(), self.len, prot) };
        assert_eq!(r, 0, "mprotect failed");
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: unmapping exactly what `new` or `stack` mapped.
        unsafe { munmap(self.ptr.sub(self.guard).cast(), self.len + self.guard) };
    }
}
