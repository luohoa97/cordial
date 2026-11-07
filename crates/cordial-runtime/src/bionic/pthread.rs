//! bionic's pthread surface, answered here rather than by a host library or a
//! stub.
//!
//! Two opposite reasons to be in this file. A type whose layout differs between
//! the libcs is *wrapped*: the bionic-sized object becomes a handle and the real
//! glibc object lives on the heap behind it. A type that is laid out the same is
//! *forwarded* straight to the host — those entry points are here only because
//! the alternative was a generated stub, and for `pthread_once` and
//! thread-specific data a stub is fatal.
//!
//! Sizes in bytes, measured rather than read off. One probe translation unit
//! declaring `char sz_x[sizeof(x)];` per type, compiled once per libc and the
//! symbol sizes read back with `nm -S`. The bionic columns are against this
//! tree's `third_party/mcpelauncher-linker/bionic/libc/include` at
//! `-D__ANDROID_API__=33` and the architecture's own `-target *-linux-android`
//! triple; the glibc columns are against that architecture's own real glibc
//! (2.43, both times this was written). The x86_64 pair was the original
//! measurement; the aarch64 pair was added on 2026-09-23, compiled x86_64
//! natively and aarch64 inside a Fedora 44 container under qemu emulation,
//! since this host has no aarch64 glibc-devel of its own — cross-checked
//! against `grep __SIZEOF_PTHREAD_MUTEX_T /usr/include/bits/pthreadtypes-arch.h`
//! inside that container, which agrees with the `nm` reading exactly.
//!
//! | type                  | bionic (x86_64) | glibc (x86_64) | bionic (aarch64) | glibc (aarch64) | treatment |
//! |-----------------------|----------------:|---------------:|------------------:|------------------:|-|
//! | `pthread_mutex_t`     |              40 |             40 |                 40 |         **48** | passthrough on x86_64, **wrapped on aarch64** — see below |
//! | `pthread_rwlock_t`    |              56 |             56 |                 56 |             56 | passthrough |
//! | `pthread_attr_t`      |              56 |             56 |                 56 |         **64** | passthrough on x86_64, **wrapped on aarch64** — see below |
//! | `pthread_once_t`      |               4 |              4 |                  4 |              4 | forwarded |
//! | `pthread_key_t`       |               4 |              4 |                  4 |              4 | forwarded |
//! | `pthread_cond_t`      |              48 |             48 |                 48 |             48 | wrapped, on a size that was wrong — see below |
//! | **`sem_t`**           |          **16** |         **32** |             **16** |         **32** | **wrapped** |
//!
//! `sem_t` is a real mismatch on both architectures: glibc's `sem_init` writes
//! 32 bytes into a 16-byte object, so the 16 bytes after it belong to whatever
//! was adjacent. The wrapper is what stops that.
//!
//! **bionic's own sizes are identical across architectures**, and that is not
//! a coincidence worth reading much into: every one of these types is declared
//! in terms of `__LP64__` in
//! `third_party/mcpelauncher-linker/bionic/libc/include/bits/pthread_types.h`,
//! not the specific processor, and both `x86_64-android` and `aarch64-android`
//! are LP64. What varies by architecture is glibc, and on x86_64 it happened
//! to agree with bionic for `pthread_mutex_t` and `pthread_attr_t` — which is
//! exactly what made the x86_64-only measurement look more reassuring than it
//! was; nothing here had ever checked whether that agreement was structural or
//! a coincidence of one architecture's glibc.
//!
//! **It was a coincidence. `pthread_mutex_t` and `pthread_attr_t` were a real
//! size mismatch on aarch64, and this was found before either was wrapped.**
//! glibc's aarch64 `pthread_mutex_t` is 48 bytes against bionic's 40; its
//! `pthread_attr_t` is 64 against bionic's 56. Roblox's engine allocates
//! these objects at bionic's size, because that is the only size it knows,
//! so handing either straight to glibc's `pthread_mutex_init` /
//! `pthread_mutex_lock` / … or `pthread_attr_init` / … meant the host libc
//! read and wrote past the end of an object the engine allocated smaller —
//! the same class of overrun `sem_t` already needed a wrapper for, just one
//! that happened not to bite on x86_64. `pthread_mutex_t` is the engine's
//! most heavily used pthread primitive, so this was never a corner case; it
//! was whichever mutex got locked first.
//!
//! **Both are wrapped now**, `BionicMutex`/`BionicAttr` further down this
//! file, aarch64 only — on x86_64 neither type is in [`overrides`] at all,
//! and `pthread_mutex_*`/`pthread_attr_*` keep resolving straight from the
//! host exactly as before this paragraph existed. The mutex wrapper has to
//! handle a mutex that was never explicitly initialised: bionic spells
//! `PTHREAD_MUTEX_INITIALIZER`, `_RECURSIVE_MUTEX_INITIALIZER_NP` and
//! `_ERRORCHECK_MUTEX_INITIALIZER_NP` as three different raw words in the
//! object's own memory, and each has to keep behaving as its declared type
//! on the very first lock, having never passed through `pthread_mutex_init`
//! — see `resolve_mutex`'s own comment for how that differs from
//! `pthread_cond_t`'s and `sem_t`'s single-sentinel lazy init below. The attr
//! wrapper also has to follow `attr` through to `pthread_create` (there is
//! no POSIX static initialiser for an attr, so it has no equivalent
//! complication) — `native/thread_trace.cpp`'s `cordial_pthread_create` calls
//! back into this file, on aarch64 only, to resolve `attr` before forwarding
//! it to the host's own `pthread_create`.
//!
//! **Run, not just typechecked, as of 2026-09-23**: `cargo test --release -p
//! cordial-runtime --lib bionic::pthread::` inside the same emulated aarch64
//! container the size measurement used, natively (the container is aarch64;
//! nothing here is cross-compiled) -- 14 passed, 0 failed, covering a
//! statically-initialised normal/recursive/errorcheck mutex each, a mutex
//! locked and unlocked by eight threads at once, an attr's stack size read
//! back through its own getter, and the same value read back on the far side
//! of a real `pthread_create`/`pthread_getattr_np` round trip through
//! `native/thread_trace.cpp`. **Still not run**: against real ARM64
//! hardware, or against Roblox's own engine -- an emulated qemu-user
//! container proves the ABI translation is right, not that the engine's
//! actual lock usage pattern is.
//!
//! **The `pthread_cond_t` row used to read 32 against 48, and it was wrong.**
//! 32 bytes is `pthread_barrier_t`, which is `int64_t __private[4]`;
//! `pthread_cond_t` is `int32_t __private[12]`, a few declarations further down
//! the same header, and comes to 48 on LP64 — the same as glibc's. The commit
//! that introduced this module recorded the overrun as one of three ABI
//! divergences found, and the measurement above says there was no overrun to
//! find. The wrapper stays for now: it is harmless either way, since it only
//! ever writes the first 12 bytes of the caller's object, and taking it out
//! changes what runs at every `pthread_cond_wait` in the engine — a behaviour
//! change that wants its own measurement rather than a free ride on this one.
//!
//! Initialisation of a wrapper is lazy because bionic's
//! `PTHREAD_COND_INITIALIZER` is all zeroes, so a statically-initialised
//! condition variable can reach `wait` without `init` ever being called.

use crate::ffi_util::{overlay_ref, store_out};
use std::ffi::{c_int, c_uint, c_void};
#[cfg(target_arch = "aarch64")]
use std::ffi::c_ulong;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};

/// Marks a wrapper whose backing object exists. Arbitrary, but distinctive in a
/// memory dump and impossible to reach by zero-initialisation.
const READY: u32 = 0xC0D1_A1FF;

const UNINIT: u32 = 0;
const INITIALISING: u32 = 1;

/// An overlay on all 48 bytes of bionic's `pthread_cond_t`, which is
/// `int32_t __private[12]` and so only promises four-byte alignment. Only the
/// first 12 bytes -- `state` and the pointer split across `real_lo` and
/// `real_hi` -- are ours; the reserved words are never touched. The fields are
/// `u32` rather than `u64`/`usize` because reading an eight-byte atomic out of
/// four-byte-aligned storage is undefined behaviour.
#[repr(C)]
struct BionicCond {
    state: AtomicU32,
    real_lo: AtomicU32,
    real_hi: AtomicU32,
    _reserved: [u32; 9],
}

/// bionic's `sem_t`: `unsigned int count; int __reserved[3];`
#[repr(C)]
struct BionicSem {
    state: AtomicU32,
    real_lo: AtomicU32,
    real_hi: AtomicU32,
    _reserved: u32,
}

// glibc's implementations. These resolve to the host's libc at link time; our
// own wrappers are never exported, so there is no recursion.
unsafe extern "C" {
    fn pthread_cond_init(cond: *mut c_void, attr: *const c_void) -> c_int;
    fn pthread_cond_destroy(cond: *mut c_void) -> c_int;
    fn pthread_cond_wait(cond: *mut c_void, mutex: *mut c_void) -> c_int;
    fn pthread_cond_timedwait(cond: *mut c_void, mutex: *mut c_void, ts: *const c_void) -> c_int;
    fn pthread_cond_signal(cond: *mut c_void) -> c_int;
    fn pthread_cond_broadcast(cond: *mut c_void) -> c_int;

    fn sem_init(sem: *mut c_void, pshared: c_int, value: u32) -> c_int;
    fn sem_destroy(sem: *mut c_void) -> c_int;
    fn sem_post(sem: *mut c_void) -> c_int;
    fn sem_wait(sem: *mut c_void) -> c_int;
    fn sem_trywait(sem: *mut c_void) -> c_int;
}

// glibc's mutex/attr/sched implementations, needed only on aarch64 -- see the
// module doc comment's table. Cfg'd out entirely on x86_64, where none of
// this is linked or called: `overrides()` never registers
// `pthread_mutex_*`/`pthread_attr_*`/`pthread_getattr_np` there, so the
// host's own implementations keep resolving directly, unchanged from before
// this fix.
#[cfg(target_arch = "aarch64")]
unsafe extern "C" {
    fn pthread_mutex_init(mutex: *mut c_void, attr: *const c_void) -> c_int;
    fn pthread_mutex_destroy(mutex: *mut c_void) -> c_int;
    fn pthread_mutex_lock(mutex: *mut c_void) -> c_int;
    fn pthread_mutex_trylock(mutex: *mut c_void) -> c_int;
    fn pthread_mutex_unlock(mutex: *mut c_void) -> c_int;
    fn pthread_mutex_timedlock(mutex: *mut c_void, abstime: *const c_void) -> c_int;

    fn pthread_mutexattr_init(attr: *mut c_void) -> c_int;
    fn pthread_mutexattr_destroy(attr: *mut c_void) -> c_int;
    fn pthread_mutexattr_settype(attr: *mut c_void, kind: c_int) -> c_int;

    fn pthread_attr_init(attr: *mut c_void) -> c_int;
    fn pthread_attr_destroy(attr: *mut c_void) -> c_int;
    fn pthread_attr_setstacksize(attr: *mut c_void, stacksize: usize) -> c_int;
    fn pthread_attr_getstacksize(attr: *const c_void, stacksize: *mut usize) -> c_int;
    fn pthread_attr_setdetachstate(attr: *mut c_void, state: c_int) -> c_int;
    fn pthread_attr_getdetachstate(attr: *const c_void, state: *mut c_int) -> c_int;
    fn pthread_attr_setguardsize(attr: *mut c_void, guardsize: usize) -> c_int;
    fn pthread_attr_getguardsize(attr: *const c_void, guardsize: *mut usize) -> c_int;
    fn pthread_attr_setschedparam(attr: *mut c_void, param: *const c_void) -> c_int;
    fn pthread_attr_getschedparam(attr: *const c_void, param: *mut c_void) -> c_int;
    fn pthread_attr_setstack(attr: *mut c_void, stackaddr: *mut c_void, stacksize: usize) -> c_int;
    fn pthread_attr_getstack(
        attr: *const c_void,
        stackaddr: *mut *mut c_void,
        stacksize: *mut usize,
    ) -> c_int;
    fn pthread_getattr_np(thread: c_ulong, attr: *mut c_void) -> c_int;
}

/// Size of the heap allocation standing in for a glibc object. Generous on
/// purpose: it costs nothing and removes any chance of repeating the very bug
/// this module fixes if a libc grows its type.
const BACKING_SIZE: usize = 128;

fn alloc_backing() -> *mut c_void {
    let boxed: Box<[u8; BACKING_SIZE]> = Box::new([0u8; BACKING_SIZE]);
    Box::into_raw(boxed) as *mut c_void
}

/// SAFETY: `ptr` must have come from `alloc_backing` and not been freed.
unsafe fn free_backing(ptr: *mut c_void) {
    // SAFETY: `ptr` came from `alloc_backing` and has not been freed, per
    // this function's own contract, stated above.
    drop(unsafe { Box::from_raw(ptr as *mut [u8; BACKING_SIZE]) });
}

/// What `resolve` and `destroy_backing` need to know about a wrapper: the three
/// words that carry its lazy-init protocol, and the glibc destructor for the
/// object those words end up pointing at.
///
/// Both functions used to take the three words as separate arguments, and
/// `destroy_backing` took the destructor as another, all under a comment saying
/// the caller must pass words from the same wrapper and the destructor that
/// matched what had been created behind it. Nothing checked either. Putting the
/// pairing in the type means the only way to get it wrong is a wrong `impl`
/// below, of which there are four, and it is what lets the two functions be
/// ordinary safe functions.
trait Wrapper {
    fn state(&self) -> &AtomicU32;
    fn real_lo(&self) -> &AtomicU32;
    fn real_hi(&self) -> &AtomicU32;
    /// glibc's destroy for the object `resolve` created behind this wrapper.
    const DESTROY: unsafe extern "C" fn(*mut c_void) -> c_int;
}

macro_rules! wrapper {
    ($ty:ty, $destroy:ident) => {
        impl Wrapper for $ty {
            fn state(&self) -> &AtomicU32 {
                &self.state
            }
            fn real_lo(&self) -> &AtomicU32 {
                &self.real_lo
            }
            fn real_hi(&self) -> &AtomicU32 {
                &self.real_hi
            }
            const DESTROY: unsafe extern "C" fn(*mut c_void) -> c_int = $destroy;
        }
    };
}

wrapper!(BionicCond, pthread_cond_destroy);
wrapper!(BionicSem, sem_destroy);

/// Resolve the real object behind a wrapper, creating it on first use.
///
/// `init` is called exactly once, with the freshly allocated backing store.
/// Returns null if the wrapper's state is a value this file never wrote.
fn resolve<W: Wrapper>(w: &W, init: impl FnOnce(*mut c_void)) -> *mut c_void {
    let (state, real_lo, real_hi) = (w.state(), w.real_lo(), w.real_hi());
    loop {
        match state.compare_exchange(UNINIT, INITIALISING, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => {
                let backing = alloc_backing();
                init(backing);
                let address = backing as usize as u64;
                real_lo.store(address as u32, Ordering::Relaxed);
                real_hi.store((address >> 32) as u32, Ordering::Relaxed);
                state.store(READY, Ordering::Release);
                return backing;
            }
            Err(READY) => {
                let address = (real_lo.load(Ordering::Relaxed) as u64)
                    | ((real_hi.load(Ordering::Relaxed) as u64) << 32);
                return address as usize as *mut c_void;
            }
            Err(INITIALISING) => {
                // Another thread is between allocation and publication. This
                // window is a handful of instructions.
                std::hint::spin_loop();
            }
            Err(_) => {
                // Not a value we wrote. The object was not zero-initialised and
                // is not one of ours — most likely a real bug in the caller, but
                // treating it as uninitialised would leak and corrupt. Refuse.
                return std::ptr::null_mut();
            }
        }
    }
}

/// Tear down and free a wrapper's backing object, if it has one.
fn destroy_backing<W: Wrapper>(w: &W) {
    if w.state().swap(UNINIT, Ordering::AcqRel) == READY {
        let low = w.real_lo().swap(0, Ordering::AcqRel) as u64;
        let high = w.real_hi().swap(0, Ordering::Acquire) as u64;
        let p = (low | (high << 32)) as usize as *mut c_void;
        if !p.is_null() {
            // SAFETY: `p` was the backing object `resolve` created for this
            // wrapper type, so `W::DESTROY` is the destructor that matches it,
            // and the `state.swap` above already claimed it -- no other caller
            // can observe `READY` again for the same wrapper, so this is the
            // one place that destroys and frees it.
            unsafe {
                (W::DESTROY)(p);
                free_backing(p);
            }
        }
    }
}

/// An explicit init on an object we already wrapped replaces it, matching
/// glibc's "undefined behaviour, but do something sane" posture.
fn reinit<W: Wrapper>(w: &W, init: impl FnOnce(*mut c_void)) -> *mut c_void {
    destroy_backing(w);
    w.state().store(UNINIT, Ordering::Release);
    resolve(w, init)
}

/// 0 if `resolve` produced a backing object, EINVAL if it refused the wrapper.
fn status_of(backing: *mut c_void) -> c_int {
    if backing.is_null() {
        libc_einval()
    } else {
        0
    }
}

// ---------------------------------------------------------------- condition vars

/// # Safety
///
/// `cond` must point at storage at least as large as bionic's
/// `pthread_cond_t` (48 bytes; see [`BionicCond`]); `attr`, if non-null, must
/// point at a `pthread_condattr_t` the host's `pthread_cond_init` can read.
/// Both hold here: bionic code only ever hands this its own statically- or
/// explicitly-declared `pthread_cond_t` and `pthread_condattr_t`.
pub unsafe extern "C" fn cond_init(cond: *mut c_void, attr: *const c_void) -> c_int {
    // SAFETY: bionic's contract is a pointer to a 48-byte pthread_cond_t.
    let Some(c) = (unsafe { overlay_ref::<BionicCond>(cond) }) else {
        return libc_einval();
    };
    cond_init_on(c, attr)
}

fn cond_init_on(c: &BionicCond, attr: *const c_void) -> c_int {
    status_of(reinit(c, |p| {
        // SAFETY: `p` is a fresh BACKING_SIZE block, far larger than glibc's
        // `pthread_cond_t`; `attr` is null or the caller's condattr, per
        // `cond_init`'s own contract.
        unsafe { pthread_cond_init(p, attr) };
    }))
}

pub extern "C" fn cond_destroy(cond: *mut c_void) -> c_int {
    // SAFETY: as above.
    let Some(c) = (unsafe { overlay_ref::<BionicCond>(cond) }) else {
        return libc_einval();
    };
    destroy_backing(c);
    0
}

macro_rules! cond_op {
    ($name:ident, $glibc:ident) => {
        pub extern "C" fn $name(cond: *mut c_void) -> c_int {
            // SAFETY: bionic's contract is a pointer to a 48-byte pthread_cond_t.
            let Some(c) = (unsafe { overlay_ref::<BionicCond>(cond) }) else {
                return libc_einval();
            };
            on_cond(c, $glibc)
        }
    };
}

cond_op!(cond_signal, pthread_cond_signal);
cond_op!(cond_broadcast, pthread_cond_broadcast);

/// Run a glibc condition-variable operation on the real object behind `c`.
fn on_cond(c: &BionicCond, op: unsafe extern "C" fn(*mut c_void) -> c_int) -> c_int {
    let Some(backing) = cond_backing(c) else {
        return libc_einval();
    };
    // SAFETY: `backing` is the live glibc `pthread_cond_t` that `resolve`
    // created behind `c`, which is what every `op` passed here takes.
    unsafe { op(backing) }
}

/// # Safety
///
/// `cond` must be a bionic `pthread_cond_t` this module has already seen
/// (statically- or explicitly-initialised); `mutex` must point at a bionic
/// `pthread_mutex_t` (40 bytes). See [`wait_mutex`] for which object glibc is
/// actually handed.
pub unsafe extern "C" fn cond_wait(cond: *mut c_void, mutex: *mut c_void) -> c_int {
    // SAFETY: bionic's contract is a pointer to a 48-byte pthread_cond_t.
    let Some(c) = (unsafe { overlay_ref::<BionicCond>(cond) }) else {
        return libc_einval();
    };
    cond_wait_on(c, mutex)
}

fn cond_wait_on(c: &BionicCond, mutex: *mut c_void) -> c_int {
    let Some(backing) = cond_backing(c) else {
        return libc_einval();
    };
    let Some(mutex) = wait_mutex(mutex) else {
        return libc_einval();
    };
    // SAFETY: `backing` is the live glibc condvar behind `c`, and `mutex` is
    // glibc's own mutex: the bionic object itself on x86-64, where the two
    // layouts agree, and its backing object on aarch64.
    unsafe { pthread_cond_wait(backing, mutex) }
}

/// The mutex glibc's condition-variable wait should release and retake.
///
/// On aarch64 this has to be the wrapper's backing object, never the bionic
/// `pthread_mutex_t` itself. Passing the bionic object through -- which is
/// what this did while the comment above it said "layout-identical to glibc's
/// on x86-64" -- let glibc "unlock" memory nobody had locked, wait, and then
/// lock it in glibc's own encoding, while the backing mutex the caller really
/// held stayed locked for the whole wait. The next wrapped operation found
/// `__lock=2` and the waiter's tid in words bionic reserves, refused them as
/// corruption with EINVAL, and libc++ turned that into an uncaught
/// `std::system_error("mutex lock failed")` and `abort()`. Seen on the Quest
/// build's first app-bridge run under qemu-aarch64, four runs of four, 125 s
/// in; the words, reported by `report_mutex_failure`, were `0x00000002 0xfc001e00 0x00020754`.
fn wait_mutex(mutex: *mut c_void) -> Option<*mut c_void> {
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: bionic's contract is a pointer to a 40-byte pthread_mutex_t.
        unsafe { overlay_ref::<BionicMutex>(mutex) }.and_then(mutex_backing)
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        (!mutex.is_null()).then_some(mutex)
    }
}

/// # Safety
///
/// As [`cond_wait`], plus `abstime`, if non-null, must point at a
/// `struct timespec`, which is identical between the two libcs.
pub unsafe extern "C" fn cond_timedwait(
    cond: *mut c_void,
    mutex: *mut c_void,
    abstime: *const c_void,
) -> c_int {
    // SAFETY: bionic's contract is a pointer to a 48-byte pthread_cond_t.
    let Some(c) = (unsafe { overlay_ref::<BionicCond>(cond) }) else {
        return libc_einval();
    };
    cond_timedwait_on(c, mutex, abstime)
}

fn cond_timedwait_on(c: &BionicCond, mutex: *mut c_void, abstime: *const c_void) -> c_int {
    let Some(backing) = cond_backing(c) else {
        return libc_einval();
    };
    let Some(mutex) = wait_mutex(mutex) else {
        return libc_einval();
    };
    // SAFETY: as `cond_wait_on`; `struct timespec` is identical between the
    // two libcs, and `abstime` is the caller's, forwarded for glibc to read.
    unsafe { pthread_cond_timedwait(backing, mutex, abstime) }
}

fn cond_backing(c: &BionicCond) -> Option<*mut c_void> {
    let backing = resolve(c, |p| {
        // SAFETY: `p` is a fresh BACKING_SIZE block; null selects the default attributes.
        unsafe { pthread_cond_init(p, std::ptr::null()) };
    });
    (!backing.is_null()).then_some(backing)
}

// ------------------------------------------------------------------ semaphores

pub extern "C" fn semaphore_init(sem: *mut c_void, pshared: c_int, value: u32) -> c_int {
    // SAFETY: bionic's contract is a pointer to a 16-byte sem_t.
    let Some(s) = (unsafe { overlay_ref::<BionicSem>(sem) }) else {
        return libc_einval();
    };
    semaphore_init_on(s, pshared, value)
}

fn semaphore_init_on(s: &BionicSem, pshared: c_int, value: u32) -> c_int {
    status_of(reinit(s, |p| {
        // SAFETY: `p` is a fresh BACKING_SIZE block, far larger than glibc's `sem_t`.
        unsafe { sem_init(p, pshared, value) };
    }))
}

pub extern "C" fn semaphore_destroy(sem: *mut c_void) -> c_int {
    // SAFETY: as above.
    let Some(s) = (unsafe { overlay_ref::<BionicSem>(sem) }) else {
        return libc_einval();
    };
    destroy_backing(s);
    0
}

macro_rules! sem_op {
    ($name:ident, $glibc:ident) => {
        pub extern "C" fn $name(sem: *mut c_void) -> c_int {
            // SAFETY: bionic's contract is a pointer to a 16-byte sem_t.
            let Some(s) = (unsafe { overlay_ref::<BionicSem>(sem) }) else {
                return libc_einval();
            };
            on_sem(s, $glibc)
        }
    };
}

sem_op!(semaphore_post, sem_post);
sem_op!(semaphore_wait, sem_wait);
sem_op!(semaphore_trywait, sem_trywait);

/// Run a glibc semaphore operation on the real object behind `s`.
fn on_sem(s: &BionicSem, op: unsafe extern "C" fn(*mut c_void) -> c_int) -> c_int {
    // Unlike condition variables a semaphore has no static initialiser,
    // so reaching here uninitialised means sem_init was skipped. Create
    // a zero-count semaphore rather than crashing.
    let backing = resolve(s, |p| {
        // SAFETY: `p` is a fresh BACKING_SIZE block, far larger than glibc's `sem_t`.
        unsafe { sem_init(p, 0, 0) };
    });
    if backing.is_null() {
        return libc_einval();
    }
    // SAFETY: `backing` is the live glibc `sem_t` that `resolve` created behind `s`.
    unsafe { op(backing) }
}

// ------------------------------------------------------------------- mutexes
//
// aarch64 only. glibc's `pthread_mutex_t` is 48 bytes there against bionic's
// 40 (see the size table above); on x86_64 the two agree and this section
// does not exist -- `pthread_mutex_*` is not in `overrides()` there, so the
// host's own implementation keeps resolving directly, exactly as it always
// has for this, the engine's hottest lock.

/// An overlay on all 40 bytes of bionic's `pthread_mutex_t`
/// (`third_party/mcpelauncher-linker/bionic/libc/include/bits/pthread_types.h`:
/// `int32_t __private[10]` on LP64). Same shape as `BionicCond`/`BionicSem`
/// above: `state` carries this file's own lazy-init protocol, `real_lo`/
/// `real_hi` split the backing pointer across two 32-bit atomics because the
/// object only promises four-byte alignment, and the rest is never touched.
#[cfg(target_arch = "aarch64")]
#[repr(C)]
struct BionicMutex {
    state: AtomicU32,
    real_lo: AtomicU32,
    real_hi: AtomicU32,
    _reserved: [u32; 7],
}

#[cfg(target_arch = "aarch64")]
wrapper!(BionicMutex, pthread_mutex_destroy);

/// glibc's own mutex-type numbering. It happens to use the same small
/// integers as bionic's `pthread_mutex.cpp` -- 0/1/2 -- but the two are
/// otherwise unrelated enums; the agreement is not load-bearing anywhere else
/// in this file, only convenient for this comment.
#[cfg_attr(not(target_arch = "aarch64"), allow(dead_code))]
const GLIBC_MUTEX_RECURSIVE: c_int = 1;
#[cfg_attr(not(target_arch = "aarch64"), allow(dead_code))]
const GLIBC_MUTEX_ERRORCHECK: c_int = 2;

/// Which mutex type a bionic mutex's *untouched* `state` word asks for.
///
/// Not cfg'd to aarch64 with the rest of the mutex section, because
/// translating the word is a pure function and the one part of this that can
/// be tested on the architecture the tests run on.
#[cfg_attr(not(target_arch = "aarch64"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StaticMutexKind {
    Normal,
    Recursive,
    ErrorCheck,
}

impl StaticMutexKind {
    /// glibc's `PTHREAD_MUTEX_*` for this kind, or `None` for the default,
    /// which needs no attribute object at all.
    #[cfg_attr(not(target_arch = "aarch64"), allow(dead_code))]
    fn glibc_type(self) -> Option<c_int> {
        match self {
            StaticMutexKind::Normal => None,
            StaticMutexKind::Recursive => Some(GLIBC_MUTEX_RECURSIVE),
            StaticMutexKind::ErrorCheck => Some(GLIBC_MUTEX_ERRORCHECK),
        }
    }
}

/// bionic's three possible "nobody has called `pthread_mutex_init` yet"
/// values for a mutex's `state` word, straight out of
/// `third_party/mcpelauncher-linker/bionic/libc/bionic/pthread_mutex.cpp`'s
/// `MUTEX_TYPE_SHIFT` (14): `PTHREAD_MUTEX_INITIALIZER`,
/// `_RECURSIVE_MUTEX_INITIALIZER_NP` and `_ERRORCHECK_MUTEX_INITIALIZER_NP`
/// spell `(type & 3) << 14` for type 0, 1 and 2 respectively. Nothing else
/// can be sitting in this word before this file's own wrapper has touched
/// it -- bionic's real mutex algorithm never runs against memory this shim
/// intercepts, and there is no fourth static-initialiser macro to account
/// for.
#[cfg_attr(not(target_arch = "aarch64"), allow(dead_code))]
fn static_mutex_kind(raw: u32) -> Option<StaticMutexKind> {
    match raw {
        0 => Some(StaticMutexKind::Normal),
        0x4000 => Some(StaticMutexKind::Recursive),
        0x8000 => Some(StaticMutexKind::ErrorCheck),
        _ => None,
    }
}

/// Resolve a mutex's backing object, creating it from bionic's own static
/// pattern the first time it is touched with no explicit `pthread_mutex_init`
/// -- see `static_mutex_kind`. Shaped like [`resolve`] but keyed off three
/// possible "not yet created" values instead of one, because a bionic
/// mutex's untouched state legitimately differs by declared type: an engine
/// mutex declared `PTHREAD_RECURSIVE_MUTEX_INITIALIZER_NP` must behave as a
/// recursive mutex on its very first lock, having never passed through
/// `pthread_mutex_init`.
#[cfg(target_arch = "aarch64")]
fn resolve_mutex(m: &BionicMutex) -> *mut c_void {
    loop {
        let raw = m.state.load(Ordering::Acquire);
        if raw == READY {
            let low = m.real_lo.load(Ordering::Relaxed) as u64;
            let high = m.real_hi.load(Ordering::Relaxed) as u64;
            return (low | (high << 32)) as usize as *mut c_void;
        }
        if raw == INITIALISING {
            // Another thread is between allocation and publication.
            std::hint::spin_loop();
            continue;
        }
        let Some(kind) = static_mutex_kind(raw) else {
            // Not one of the three static patterns and not our own
            // INITIALISING/READY markers -- either genuine corruption or a
            // caller that hand-wrote a bit pattern this shim was never told
            // about. Refuse rather than guess a type, the same posture
            // `resolve`'s own catch-all takes for cond/sem.
            return std::ptr::null_mut();
        };
        if m
            .state
            .compare_exchange(raw, INITIALISING, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            // Lost a race with another thread also resolving this mutex for
            // the first time; reobserve rather than assume who won.
            continue;
        }
        let backing = alloc_backing();
        // SAFETY: `backing` is BACKING_SIZE (128) bytes, comfortably larger
        // than glibc's real `pthread_mutex_t` (48 on aarch64) and its
        // `pthread_mutexattr_t` (a `long`-sized union on any LP64 host).
        unsafe {
            match kind.glibc_type() {
                None => {
                    pthread_mutex_init(backing, std::ptr::null());
                }
                Some(glibc_kind) => {
                    let mut attr_storage = [0u8; BACKING_SIZE];
                    let attr = attr_storage.as_mut_ptr() as *mut c_void;
                    pthread_mutexattr_init(attr);
                    pthread_mutexattr_settype(attr, glibc_kind);
                    pthread_mutex_init(backing, attr);
                    pthread_mutexattr_destroy(attr);
                }
            }
        }
        let address = backing as usize as u64;
        m.real_lo.store(address as u32, Ordering::Relaxed);
        m.real_hi.store((address >> 32) as u32, Ordering::Relaxed);
        m.state.store(READY, Ordering::Release);
        return backing;
    }
}

#[cfg(target_arch = "aarch64")]
pub extern "C" fn mutex_init(mutex: *mut c_void, attr: *const c_void) -> c_int {
    // SAFETY: bionic's contract is a pointer to a 40-byte pthread_mutex_t.
    let Some(m) = (unsafe { overlay_ref::<BionicMutex>(mutex) }) else {
        return libc_einval();
    };
    mutex_init_on(m, attr)
}

#[cfg(target_arch = "aarch64")]
fn mutex_init_on(m: &BionicMutex, attr: *const c_void) -> c_int {
    // An explicit init on an object we already wrapped replaces it, matching
    // `cond_init`'s posture on the same situation.
    //
    // `attr` is passed straight through here, unlike `resolve_mutex`'s
    // static-pattern path: an explicitly supplied attr was built by the
    // engine calling `pthread_mutexattr_init`/`_settype`, which this file
    // does not intercept, because bionic's `pthread_mutexattr_t` and
    // glibc's are both a single `long`-sized word on LP64 -- whichever libc's
    // functions wrote it, wrote a self-consistent encoding that the *same*
    // libc's `pthread_mutex_init` reads correctly. Since those attr
    // functions are left unintercepted (ordinary passthrough, resolved from
    // the host), `attr` already holds glibc's own encoding by the time it
    // reaches here, not bionic's, and needs no translation.
    status_of(reinit(m, |p| {
        // SAFETY: `p` is a fresh BACKING_SIZE block, far larger than glibc's
        // `pthread_mutex_t`; `attr` is null or the caller's mutexattr.
        unsafe { pthread_mutex_init(p, attr) };
    }))
}

#[cfg(target_arch = "aarch64")]
pub extern "C" fn mutex_destroy(mutex: *mut c_void) -> c_int {
    // SAFETY: as above.
    let Some(m) = (unsafe { overlay_ref::<BionicMutex>(mutex) }) else {
        return libc_einval();
    };
    destroy_backing(m);
    0
}

#[cfg(target_arch = "aarch64")]
fn mutex_backing(m: &BionicMutex) -> Option<*mut c_void> {
    let backing = resolve_mutex(m);
    (!backing.is_null()).then_some(backing)
}

/// The ten words of a mutex, as `report_mutex_failure` prints them.
#[cfg_attr(not(target_arch = "aarch64"), allow(dead_code))]
fn format_words(words: &[u32]) -> String {
    words.iter().map(|x| format!("{x:#010x}")).collect::<Vec<_>>().join(" ")
}

/// Say what a failing mutex operation saw, on stderr, every time.
///
/// libc++'s `std::mutex::lock` turns any nonzero return into
/// `std::system_error("mutex lock failed")`, which the engine does not catch,
/// so the first sign of a failure here used to be a bare `abort()` with no
/// message -- the Quest build's first app-bridge run under qemu died exactly
/// that way, and only a stack scan found the string. It fires only on the
/// path that is about to end the process, so it costs nothing otherwise.
#[cfg(target_arch = "aarch64")]
fn report_mutex_failure(op: &str, mutex: *mut c_void, m: Option<&BionicMutex>, rc: c_int) {
    let words = m.map_or_else(
        || String::from("null"),
        |m| {
            let mut w = [0u32; 10];
            w[0] = m.state.load(Ordering::Relaxed);
            w[1] = m.real_lo.load(Ordering::Relaxed);
            w[2] = m.real_hi.load(Ordering::Relaxed);
            w[3..].copy_from_slice(&m._reserved);
            format_words(&w)
        },
    );
    eprintln!("[pthread] {op}({mutex:p}) -> {rc}; bionic words: {words}");
}

#[cfg(target_arch = "aarch64")]
macro_rules! mutex_op {
    ($name:ident, $glibc:ident) => {
        pub extern "C" fn $name(mutex: *mut c_void) -> c_int {
            // SAFETY: bionic's contract is a pointer to a 40-byte pthread_mutex_t.
            let m = unsafe { overlay_ref::<BionicMutex>(mutex) };
            on_mutex(m, mutex, stringify!($glibc), $glibc)
        }
    };
}

/// Run a glibc mutex operation on the real object behind `m`, reporting a
/// refusal or an error to stderr. `ptr` is the caller's pointer, only for the
/// report.
#[cfg(target_arch = "aarch64")]
fn on_mutex(
    m: Option<&BionicMutex>,
    ptr: *mut c_void,
    name: &str,
    op: unsafe extern "C" fn(*mut c_void) -> c_int,
) -> c_int {
    let Some(backing) = m.and_then(mutex_backing) else {
        report_mutex_failure(name, ptr, m, libc_einval());
        return libc_einval();
    };
    // SAFETY: `backing` is the live glibc `pthread_mutex_t` behind `m`, which
    // is what every `op` passed here takes.
    let rc = unsafe { op(backing) };
    // EBUSY is trylock's ordinary answer, not a failure.
    if rc != 0 && rc != 16 {
        report_mutex_failure(name, ptr, m, rc);
    }
    rc
}

#[cfg(target_arch = "aarch64")]
mutex_op!(mutex_lock, pthread_mutex_lock);
#[cfg(target_arch = "aarch64")]
mutex_op!(mutex_trylock, pthread_mutex_trylock);
#[cfg(target_arch = "aarch64")]
mutex_op!(mutex_unlock, pthread_mutex_unlock);

#[cfg(target_arch = "aarch64")]
pub extern "C" fn mutex_timedlock(mutex: *mut c_void, abstime: *const c_void) -> c_int {
    // SAFETY: bionic's contract is a pointer to a 40-byte pthread_mutex_t.
    let Some(backing) = (unsafe { overlay_ref::<BionicMutex>(mutex) }).and_then(mutex_backing) else {
        return libc_einval();
    };
    // SAFETY: `backing` is the live glibc mutex behind the caller's; `struct
    // timespec` is identical between the two libcs, the same assumption
    // `cond_timedwait` already makes above.
    unsafe { pthread_mutex_timedlock(backing, abstime) }
}

// -------------------------------------------------------------- thread attrs
//
// aarch64 only -- 56 bytes in bionic against 64 in glibc; see the module doc
// comment. Unlike a mutex there is no POSIX static initialiser for an attr
// object, so this reuses [`resolve`]/[`destroy_backing`] exactly as `cond`
// and `sem` do, with one addition: `pthread_getattr_np` fills in an attr
// describing an *existing* thread rather than one the caller initialised, so
// it gets its own path below rather than going through `attr_backing`.
//
// This also means `pthread_create` needs to translate whatever `attr` it is
// given before handing it to the host's own `pthread_create` --
// `cordial_pthread_attr_real` below is exported for exactly that, and
// `native/thread_trace.cpp`'s `cordial_pthread_create` calls it on aarch64.
// Missing that call site would leave every other fix in this file correct
// and this one path -- thread creation with a non-default attr, which is
// what a set stack size goes through -- still handing glibc a 56-byte object
// it reads as its own 64-byte one.

/// An overlay on all 56 bytes of bionic's `pthread_attr_t`.
#[cfg(target_arch = "aarch64")]
#[repr(C)]
struct BionicAttr {
    state: AtomicU32,
    real_lo: AtomicU32,
    real_hi: AtomicU32,
    _reserved: [u32; 11],
}

#[cfg(target_arch = "aarch64")]
wrapper!(BionicAttr, pthread_attr_destroy);

#[cfg(target_arch = "aarch64")]
pub extern "C" fn attr_init(attr: *mut c_void) -> c_int {
    // SAFETY: bionic's contract is a pointer to a 56-byte pthread_attr_t.
    let Some(a) = (unsafe { overlay_ref::<BionicAttr>(attr) }) else {
        return libc_einval();
    };
    status_of(reinit(a, |p| {
        // SAFETY: `p` is a fresh BACKING_SIZE block, far larger than glibc's
        // `pthread_attr_t`.
        unsafe { pthread_attr_init(p) };
    }))
}

#[cfg(target_arch = "aarch64")]
pub extern "C" fn attr_destroy(attr: *mut c_void) -> c_int {
    // SAFETY: as above.
    let Some(a) = (unsafe { overlay_ref::<BionicAttr>(attr) }) else {
        return libc_einval();
    };
    destroy_backing(a);
    0
}

/// Resolve an attr's backing object, auto-initialising on first touch if a
/// setter or getter is reached with no explicit `pthread_attr_init` --
/// undefined behaviour under POSIX regardless of libc, so this is the same
/// permissive fallback `cond`/`sem` already take rather than a third
/// behaviour introduced just for this type.
#[cfg(target_arch = "aarch64")]
fn attr_backing(a: &BionicAttr) -> Option<*mut c_void> {
    let backing = resolve(a, |p| {
        // SAFETY: `p` is a fresh BACKING_SIZE block, far larger than glibc's
        // `pthread_attr_t`.
        unsafe { pthread_attr_init(p) };
    });
    (!backing.is_null()).then_some(backing)
}

/// Run `op` on the real glibc attr behind a bionic one, or EINVAL for a null
/// or corrupt one. Every setter and getter below is this plus one glibc call.
///
/// # Safety
///
/// `attr`, if non-null, must be a live 56-byte `pthread_attr_t`; and `op` must
/// pass the backing object it is handed only to glibc functions that take a
/// `pthread_attr_t`.
#[cfg(target_arch = "aarch64")]
unsafe fn with_attr(attr: *const c_void, op: impl FnOnce(*mut c_void) -> c_int) -> c_int {
    // SAFETY: the caller's obligation, stated above.
    match unsafe { overlay_ref::<BionicAttr>(attr) }.and_then(attr_backing) {
        Some(backing) => op(backing),
        None => libc_einval(),
    }
}

#[cfg(target_arch = "aarch64")]
pub extern "C" fn attr_setstacksize(attr: *mut c_void, stacksize: usize) -> c_int {
    // SAFETY: bionic's contract is a 56-byte pthread_attr_t, and the closure
    // hands the backing object to a glibc attr function.
    unsafe { with_attr(attr, |b| pthread_attr_setstacksize(b, stacksize)) }
}

#[cfg(target_arch = "aarch64")]
pub extern "C" fn attr_getstacksize(attr: *const c_void, out: *mut usize) -> c_int {
    // SAFETY: as above; `out` is the caller's, written by glibc.
    unsafe { with_attr(attr, |b| pthread_attr_getstacksize(b, out)) }
}

#[cfg(target_arch = "aarch64")]
pub extern "C" fn attr_setdetachstate(attr: *mut c_void, state: c_int) -> c_int {
    // SAFETY: as above.
    unsafe { with_attr(attr, |b| pthread_attr_setdetachstate(b, state)) }
}

#[cfg(target_arch = "aarch64")]
pub extern "C" fn attr_getdetachstate(attr: *const c_void, out: *mut c_int) -> c_int {
    // SAFETY: as above.
    unsafe { with_attr(attr, |b| pthread_attr_getdetachstate(b, out)) }
}

#[cfg(target_arch = "aarch64")]
pub extern "C" fn attr_setguardsize(attr: *mut c_void, guardsize: usize) -> c_int {
    // SAFETY: as above.
    unsafe { with_attr(attr, |b| pthread_attr_setguardsize(b, guardsize)) }
}

#[cfg(target_arch = "aarch64")]
pub extern "C" fn attr_getguardsize(attr: *const c_void, out: *mut usize) -> c_int {
    // SAFETY: as above.
    unsafe { with_attr(attr, |b| pthread_attr_getguardsize(b, out)) }
}

#[cfg(target_arch = "aarch64")]
pub extern "C" fn attr_setschedparam(attr: *mut c_void, param: *const c_void) -> c_int {
    // SAFETY: as above; `struct sched_param` is `{ int sched_priority; }` in
    // both libcs on Linux -- INFERRED from the POSIX-mandated field and
    // confirmed against this tree's own vendored bionic header, not
    // independently measured against glibc's the way the pthread types above
    // were.
    unsafe { with_attr(attr, |b| pthread_attr_setschedparam(b, param)) }
}

#[cfg(target_arch = "aarch64")]
pub extern "C" fn attr_getschedparam(attr: *const c_void, param: *mut c_void) -> c_int {
    // SAFETY: as above.
    unsafe { with_attr(attr, |b| pthread_attr_getschedparam(b, param)) }
}

#[cfg(target_arch = "aarch64")]
pub extern "C" fn attr_setstack(attr: *mut c_void, stackaddr: *mut c_void, stacksize: usize) -> c_int {
    // SAFETY: as above.
    unsafe { with_attr(attr, |b| pthread_attr_setstack(b, stackaddr, stacksize)) }
}

#[cfg(target_arch = "aarch64")]
pub extern "C" fn attr_getstack(
    attr: *const c_void,
    stackaddr: *mut *mut c_void,
    stacksize: *mut usize,
) -> c_int {
    // SAFETY: as above.
    unsafe { with_attr(attr, |b| pthread_attr_getstack(b, stackaddr, stacksize)) }
}

#[cfg(target_arch = "aarch64")]
pub extern "C" fn getattr_np(thread: c_ulong, attr: *mut c_void) -> c_int {
    // SAFETY: bionic's contract is a pointer to a 56-byte pthread_attr_t.
    let Some(a) = (unsafe { overlay_ref::<BionicAttr>(attr) }) else {
        return libc_einval();
    };
    getattr_np_on(a, thread)
}

#[cfg(target_arch = "aarch64")]
fn getattr_np_on(a: &BionicAttr, thread: c_ulong) -> c_int {
    // Unlike every other attr function, this one *writes* the attr rather
    // than reading one the caller built -- so any existing backing is
    // replaced, the same posture `mutex_init`/`cond_init` take on an
    // explicit re-init, rather than reused.
    let mut result: c_int = 0;
    let backing = reinit(a, |p| {
        // SAFETY: `p` is a fresh BACKING_SIZE block, far larger than glibc's
        // `pthread_attr_t`; `thread` is a handle the caller obtained.
        result = unsafe { pthread_getattr_np(thread, p) };
    });
    if backing.is_null() {
        libc_einval()
    } else {
        result
    }
}

/// Resolve a `pthread_attr_t*` handed to `pthread_create` to its real glibc
/// backing object -- exported for `native/thread_trace.cpp`'s
/// `cordial_pthread_create`, which otherwise forwards `attr` straight to the
/// host's own `pthread_create`. Correct on x86_64, where bionic's and
/// glibc's `pthread_attr_t` agree in size and this function does not even
/// exist; wrong on aarch64, where that would hand glibc a 56-byte object it
/// reads as its own 64-byte one -- the same overrun class `BionicMutex`
/// exists to stop for the hottest lock, one call site later.
///
/// A null `attr` (by far the common case -- most `pthread_create` calls pass
/// none) is returned unchanged rather than resolved into anything, matching
/// glibc's own "use the default attributes" reading of null. A resolution
/// failure also falls back to the original pointer rather than null: neither
/// is right for the anomalous state that would cause it, and this at least
/// reproduces the pre-fix behaviour rather than guaranteeing a crash on top
/// of whatever corrupted the attr.
#[cfg(target_arch = "aarch64")]
#[no_mangle]
pub extern "C" fn cordial_pthread_attr_real(attr: *const c_void) -> *const c_void {
    // SAFETY: as `attr_backing` above: null, or a live 56-byte attr.
    match unsafe { overlay_ref::<BionicAttr>(attr) }.and_then(attr_backing) {
        Some(backing) => backing,
        None => attr,
    }
}

// ------------------------------------------------- once, and thread-local keys

// glibc's. Renamed so the forwarding wrappers below can carry bionic's names.
unsafe extern "C" {
    #[link_name = "pthread_once"]
    fn host_pthread_once(control: *mut c_int, init_routine: Option<extern "C" fn()>) -> c_int;
    #[link_name = "pthread_key_create"]
    fn host_pthread_key_create(
        key: *mut c_uint,
        destructor: Option<extern "C" fn(*mut c_void)>,
    ) -> c_int;
    #[link_name = "pthread_key_delete"]
    fn host_pthread_key_delete(key: c_uint) -> c_int;
    #[link_name = "pthread_getspecific"]
    fn host_pthread_getspecific(key: c_uint) -> *mut c_void;
    #[link_name = "pthread_setspecific"]
    fn host_pthread_setspecific(key: c_uint, value: *const c_void) -> c_int;
}

/// `pthread_once`, forwarded to the host's.
///
/// These five were generated stubs until now, and a generated stub returns 0.
/// For most symbols that is a harmless placeholder; for these it is the lie
/// AGENTS.md forbids, in its worst form — the caller is told it *succeeded*.
/// `pthread_once` returning 0 means "your initialiser ran", so whatever it was
/// meant to set up is uninitialised and the next access faults with no visible
/// relationship to this call; `pthread_getspecific` returning 0 is a NULL the
/// caller dereferences on the next line. `cordial-run --lib-dir DIR` without
/// `--host-libc` segfaulted at exit 139 with `[stub] pthread_once` and
/// `[stub] pthread_getspecific` as the last two lines before the core dump.
///
/// Compiling bionic's own implementations instead is right for exactly one of
/// the five, which is why none of them does it. `pthread_key.cpp` reaches
/// thread-specific data through `__get_bionic_tls()`, which is
/// `__get_tls()[TLS_SLOT_BIONIC_TLS]` — a pointer to bionic's own thread
/// structure, hanging off the thread pointer of a thread bionic created. Every
/// thread in this process belongs to the host's libc, so that slot holds
/// whatever glibc keeps at that offset and the load reads the wrong memory.
/// That is worse than the stub it replaced, because it would appear to work.
/// `pthread_once.cpp` is the exception — a compare-exchange loop on the
/// caller's own `int` plus a futex, touching no thread structure at all — so it
/// could be ported standalone. Forwarding costs less and behaves the same.
///
/// Forwarding is safe *here* for a reason that does not generalise, and reading
/// it as a general licence is how `struct stat` and `sigset_t` would get passed
/// through next. It is safe only where the argument is laid out identically in
/// both libcs. `pthread_once_t` is `int` in both, 4 bytes against 4, and both
/// spell `PTHREAD_ONCE_INIT` as 0 — so a bionic once-control that was
/// statically initialised and never passed to bionic's implementation already
/// *is* a valid glibc one. `pthread_key_t` is 4 bytes in both and is opaque to
/// the caller. Compare `sem_t` at the top of this file, 16 against 32, where
/// the same forwarding would write past the end of the object.
///
/// One difference forwarding does not hide, and it is **INFERRED** — nothing
/// has been observed depending on it. bionic sets `KEY_VALID_FLAG`, bit 31, in
/// every key it hands out, so a bionic key is always a negative `int`; glibc's
/// are small non-negative ones and the first is 0. Code that treats key 0 as
/// "no key allocated" would be wrong here in a way it never was on Android.
///
/// # Safety
///
/// `control` must point at 4 bytes of storage bionic's `pthread_once_t`
/// occupies -- a statically- or `PTHREAD_ONCE_INIT`-initialised once-control
/// that has never been passed to bionic's own implementation, per the
/// comment above.
pub unsafe extern "C" fn once(control: *mut c_int, init_routine: Option<extern "C" fn()>) -> c_int {
    // SAFETY: `control` is null or points at 4 bytes in both libcs, and nothing
    // here ever makes a `&mut` to it: threads race on a once-control by design,
    // so the borrow is of the atomic it is, not of a plain `int`.
    let Some(control) = (unsafe { overlay_ref::<AtomicI32>(control as *const c_void) }) else {
        return libc_einval();
    };
    once_on(control, init_routine)
}

fn once_on(control: &AtomicI32, init_routine: Option<extern "C" fn()>) -> c_int {
    // SAFETY: a bionic once-control that has never been passed to bionic's
    // implementation holds a state glibc's understands, and `AtomicI32` has the
    // layout of the `int` it borrows. `init_routine` is a plain `void (*)(void)`.
    unsafe { host_pthread_once(control.as_ptr(), init_routine) }
}

/// `pthread_key_create`. bionic's `pthread_key_t` is signed, glibc's is not,
/// hence the local rather than a cast of the caller's pointer — and nothing is
/// written back unless the host says it succeeded, which is bionic's contract.
///
/// # Safety
///
/// `key` must point at 4 bytes of writable storage for the caller's
/// `pthread_key_t`.
pub unsafe extern "C" fn key_create(
    key: *mut c_int,
    destructor: Option<extern "C" fn(*mut c_void)>,
) -> c_int {
    if key.is_null() {
        return libc_einval();
    }
    match new_key(destructor) {
        Ok(created) => {
            // SAFETY: non-null per the check above; the caller's
            // `pthread_key_t*`, 4 bytes in both libcs.
            unsafe { store_out(key, created) };
            0
        }
        Err(rc) => rc,
    }
}

/// The host's `pthread_key_create`, with the key converted to bionic's signed
/// type and an error status returned as the `Err`, so that nothing is ever
/// written for the caller to mistake for a key.
fn new_key(destructor: Option<extern "C" fn(*mut c_void)>) -> Result<c_int, c_int> {
    let mut host_key: c_uint = 0;
    // SAFETY: `host_key` is a live 4-byte slot; the destructor is passed through
    // untouched and glibc calls it on the same thread bionic would have.
    let rc = unsafe { host_pthread_key_create(&mut host_key, destructor) };
    if rc == 0 {
        Ok(host_key as c_int)
    } else {
        Err(rc)
    }
}

pub extern "C" fn key_delete(key: c_int) -> c_int {
    // SAFETY: a key is an opaque scalar; an invalid one is rejected by glibc.
    unsafe { host_pthread_key_delete(key as c_uint) }
}

pub extern "C" fn getspecific(key: c_int) -> *mut c_void {
    // SAFETY: as above. A key never created returns null, as bionic's does.
    unsafe { host_pthread_getspecific(key as c_uint) }
}

/// # Safety
///
/// `value` is stored and handed back by [`getspecific`], never dereferenced
/// here, so this has no requirement on it beyond being a value the caller
/// intends to get back; `key` is an opaque scalar glibc rejects if invalid.
pub unsafe extern "C" fn setspecific(key: c_int, value: *const c_void) -> c_int {
    // SAFETY: as above. `value` is stored, never dereferenced.
    unsafe { host_pthread_setspecific(key as c_uint, value) }
}

fn libc_einval() -> c_int {
    22 // EINVAL, identical in both libcs
}

/// `CORDIAL_COND_PASSTHROUGH=1`: leave `pthread_cond_*` to the host on x86_64.
///
/// **A diagnostic switch, off by default, and not a fix.** The condition
/// variable wrappers below put a heap object behind every engine `pthread_cond_t`
/// and add a lazy-init state machine in front of glibc's. The size table above
/// says the two libcs agree on the type (48 bytes, all-zero static initialiser),
/// so the wrapper was never needed on this architecture, only harmless by
/// argument -- and the startup freeze is a lost-wakeup-shaped wait. This makes
/// "is the wrapper in the way" a same-binary A/B rather than a theory. `sem_*`
/// stays wrapped either way: `sem_t` really does differ (16 against 32).
/// Only meaningful with `--host-libc`, which is what resolves the symbols this
/// stops overriding; without it they would fall to stubs.
fn cond_passthrough() -> bool {
    cfg!(target_arch = "x86_64")
        && std::env::var_os("CORDIAL_COND_PASSTHROUGH").is_some_and(|v| v == "1")
}

/// Everything this module replaces.
pub fn overrides() -> Vec<(&'static str, *mut c_void)> {
    overrides_with(cond_passthrough())
}

fn overrides_with(bypass_cond: bool) -> Vec<(&'static str, *mut c_void)> {
    macro_rules! f {
        ($name:literal, $fn:expr) => {
            ($name, $fn as *const () as *mut c_void)
        };
    }
    // `mut` is only needed for the aarch64 extension below; unused on x86_64.
    #[allow(unused_mut)]
    let mut v = vec![
        f!("pthread_cond_init", cond_init),
        f!("pthread_cond_destroy", cond_destroy),
        f!("pthread_cond_wait", cond_wait),
        f!("pthread_cond_timedwait", cond_timedwait),
        f!("pthread_cond_signal", cond_signal),
        f!("pthread_cond_broadcast", cond_broadcast),
        f!("sem_init", semaphore_init),
        f!("sem_destroy", semaphore_destroy),
        f!("sem_post", semaphore_post),
        f!("sem_wait", semaphore_wait),
        f!("sem_trywait", semaphore_trywait),
        // Forwarded, not wrapped. These are here so `--lib-dir` alone does not
        // need `--host-libc` for them; see `once` for why forwarding is right
        // for these five and wrong for the two above.
        f!("pthread_once", once),
        f!("pthread_key_create", key_create),
        f!("pthread_key_delete", key_delete),
        f!("pthread_getspecific", getspecific),
        f!("pthread_setspecific", setspecific),
    ];
    // `pthread_mutex_t` and `pthread_attr_t` need translating only on
    // aarch64 -- see the module doc comment's table. On x86_64 the two libcs
    // agree in size and these symbols are left to resolve straight from the
    // host, exactly as before this fix.
    #[cfg(target_arch = "aarch64")]
    v.extend([
        f!("pthread_mutex_init", mutex_init),
        f!("pthread_mutex_destroy", mutex_destroy),
        f!("pthread_mutex_lock", mutex_lock),
        f!("pthread_mutex_trylock", mutex_trylock),
        f!("pthread_mutex_unlock", mutex_unlock),
        f!("pthread_mutex_timedlock", mutex_timedlock),
        f!("pthread_attr_init", attr_init),
        f!("pthread_attr_destroy", attr_destroy),
        f!("pthread_attr_setstacksize", attr_setstacksize),
        f!("pthread_attr_getstacksize", attr_getstacksize),
        f!("pthread_attr_setdetachstate", attr_setdetachstate),
        f!("pthread_attr_getdetachstate", attr_getdetachstate),
        f!("pthread_attr_setguardsize", attr_setguardsize),
        f!("pthread_attr_getguardsize", attr_getguardsize),
        f!("pthread_attr_setschedparam", attr_setschedparam),
        f!("pthread_attr_getschedparam", attr_getschedparam),
        f!("pthread_attr_setstack", attr_setstack),
        f!("pthread_attr_getstack", attr_getstack),
        f!("pthread_getattr_np", getattr_np),
    ]);
    if bypass_cond {
        v.retain(|(name, _)| !name.starts_with("pthread_cond_"));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrappers_fit_inside_the_bionic_objects() {
        // A wrapper is an overlay on storage the caller allocated to bionic's
        // size, so it must never be larger than bionic's type. `sem_t` is 16
        // and the overlay uses all of it; `pthread_cond_t` is 48 and the
        // overlay spans all 48, of which it reads and writes the first 12.
        assert!(std::mem::size_of::<BionicCond>() <= 48);
        assert_eq!(std::mem::size_of::<BionicSem>(), 16);
        #[cfg(target_arch = "aarch64")]
        {
            assert_eq!(std::mem::size_of::<BionicMutex>(), 40);
            assert_eq!(std::mem::size_of::<BionicAttr>(), 56);
        }
    }

    /// A timed wait must release and retake the mutex the caller actually
    /// holds, and leave the bionic object as the wrapper left it. On aarch64
    /// the bionic object used to reach glibc directly -- see [`wait_mutex`] --
    /// and the unlock after the wait then failed with EINVAL.
    #[test]
    fn cond_timedwait_keeps_the_mutex_usable() {
        let mut cond_storage = [0u64; 6];
        let mut mutex_storage = [0u64; 5];
        let cond = cond_storage.as_mut_ptr() as *mut c_void;
        let mutex = mutex_storage.as_mut_ptr() as *mut c_void;
        let lock = mutex_lock_for_test(mutex);
        assert_eq!(lock, 0);
        // An absolute CLOCK_REALTIME deadline already in the past: the wait
        // releases the mutex, times out at once and retakes it.
        let ts = [0i64, 0i64];
        let rc = unsafe { cond_timedwait(cond, mutex, ts.as_ptr() as *const c_void) };
        assert_eq!(rc, 110, "ETIMEDOUT");
        assert_eq!(mutex_unlock_for_test(mutex), 0);
        #[cfg(target_arch = "aarch64")]
        assert_eq!(mutex_storage[0] as u32, READY);
    }

    fn mutex_lock_for_test(m: *mut c_void) -> c_int {
        #[cfg(target_arch = "aarch64")]
        {
            mutex_lock(m)
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            unsafe extern "C" {
                fn pthread_mutex_lock(mutex: *mut c_void) -> c_int;
            }
            unsafe { pthread_mutex_lock(m) }
        }
    }

    fn mutex_unlock_for_test(m: *mut c_void) -> c_int {
        #[cfg(target_arch = "aarch64")]
        {
            mutex_unlock(m)
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            unsafe extern "C" {
                fn pthread_mutex_unlock(mutex: *mut c_void) -> c_int;
            }
            unsafe { pthread_mutex_unlock(m) }
        }
    }

    #[test]
    fn cond_passthrough_drops_exactly_the_cond_overrides() {
        let names = |bypass| -> Vec<&'static str> {
            overrides_with(bypass).into_iter().map(|(n, _)| n).collect()
        };
        let wrapped = names(false);
        let bypassed = names(true);
        assert!(wrapped.contains(&"pthread_cond_wait"));
        assert!(wrapped.contains(&"pthread_cond_broadcast"));
        assert!(!bypassed.iter().any(|n| n.starts_with("pthread_cond_")));
        // Everything else, sem_* above all, is untouched by the switch.
        assert!(bypassed.contains(&"sem_wait"));
        assert!(bypassed.contains(&"pthread_once"));
        let kept: Vec<_> = wrapped.iter().filter(|n| !n.starts_with("pthread_cond_")).collect();
        assert_eq!(kept.len(), bypassed.len());
    }

    #[test]
    fn statically_initialised_cond_works() {
        // bionic's PTHREAD_COND_INITIALIZER is all zeroes; signalling one that
        // was never explicitly initialised must still work.
        // u64 storage is more aligned than the bionic object requires. The
        // wrapper itself uses only four-byte atomics because bionic condition
        // variables are allowed to start at four-byte alignment. Six words, not
        // four: the overlay is 48 bytes, and a reference to it over 32 bytes of
        // storage is undefined behaviour even though only 12 are touched.
        let mut storage = [0u64; 6];
        let cond = storage.as_mut_ptr() as *mut c_void;
        assert_eq!(cond_signal(cond), 0);
        assert_eq!(cond_broadcast(cond), 0);
        assert_eq!(cond_destroy(cond), 0);
    }

    /// The first use of a statically-initialised condition variable is a race
    /// between whoever waits and whoever signals, and the wrapper's lazy
    /// initialisation sits exactly there. A lost wakeup on that first use would
    /// look like a wait that nothing ever ends, so this hammers only the first
    /// use: a fresh, all-zero cond and mutex per iteration, several threads at
    /// once, signal issued after the lock is dropped (legal, and the ordering
    /// that leaves the most room for a wake to land before the wait begins).
    ///
    /// x86_64 only: it locks with the host's `pthread_mutex_t`, which is the
    /// bionic layout there and not on aarch64.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn first_use_race_never_loses_a_wakeup() {
        use std::sync::atomic::AtomicU32;
        use std::sync::mpsc;
        use std::time::Duration;

        unsafe extern "C" {
            fn pthread_mutex_lock(m: *mut c_void) -> c_int;
            fn pthread_mutex_unlock(m: *mut c_void) -> c_int;
        }
        #[repr(C, align(16))]
        struct Fresh {
            cond: [u64; 6],
            mutex: [u64; 5],
            flag: AtomicU32,
        }

        const WORKERS: usize = 4;
        const ROUNDS: usize = 3000;
        let (tx, rx) = mpsc::channel::<usize>();
        for w in 0..WORKERS {
            let tx = tx.clone();
            std::thread::spawn(move || {
                for round in 0..ROUNDS {
                    let fresh = Box::into_raw(Box::new(Fresh {
                        cond: [0; 6],
                        mutex: [0; 5],
                        flag: AtomicU32::new(0),
                    }));
                    let addr = fresh as usize;
                    let waiter = std::thread::spawn(move || {
                        let f = addr as *mut Fresh;
                        // SAFETY: `f` outlives both threads; the join below orders the free.
                        unsafe {
                            let m = std::ptr::addr_of_mut!((*f).mutex) as *mut c_void;
                            let c = std::ptr::addr_of_mut!((*f).cond) as *mut c_void;
                            pthread_mutex_lock(m);
                            while (*f).flag.load(Ordering::Acquire) == 0 {
                                cond_wait(c, m);
                            }
                            pthread_mutex_unlock(m);
                        }
                    });
                    // Vary who gets there first, including simultaneously.
                    for _ in 0..(round % 7) * 40 {
                        std::hint::spin_loop();
                    }
                    // SAFETY: as above.
                    unsafe {
                        let m = std::ptr::addr_of_mut!((*fresh).mutex) as *mut c_void;
                        let c = std::ptr::addr_of_mut!((*fresh).cond) as *mut c_void;
                        pthread_mutex_lock(m);
                        (*fresh).flag.store(1, Ordering::Release);
                        pthread_mutex_unlock(m);
                        if round % 2 == 0 {
                            cond_signal(c);
                        } else {
                            cond_broadcast(c);
                        }
                    }
                    waiter.join().unwrap();
                    // SAFETY: both users are done.
                    unsafe {
                        cond_destroy(std::ptr::addr_of_mut!((*fresh).cond) as *mut c_void);
                        drop(Box::from_raw(fresh));
                    }
                }
                tx.send(w).unwrap();
            });
        }
        drop(tx);
        for _ in 0..WORKERS {
            rx.recv_timeout(Duration::from_secs(60))
                .expect("a worker never finished: a wakeup was lost on a cond's first use");
        }
    }

    #[test]
    fn init_destroy_roundtrip_does_not_leak_state() {
        let mut storage = [0u64; 6];
        let cond = storage.as_mut_ptr() as *mut c_void;
        // SAFETY: `cond` is a live, correctly-sized `pthread_cond_t` on this
        // thread's stack, per `cond_init`/`cond_destroy`'s own contracts.
        unsafe {
            assert_eq!(cond_init(cond, std::ptr::null()), 0);
            assert_eq!(cond_destroy(cond), 0);
            // Destroyed wrappers return to the zero state, so they can be reused.
            assert_eq!(cond_init(cond, std::ptr::null()), 0);
            assert_eq!(cond_destroy(cond), 0);
        }
    }

    #[test]
    fn once_runs_the_initialiser_exactly_once() {
        static RUNS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        unsafe extern "C" fn init() {
            RUNS.fetch_add(1, Ordering::SeqCst);
        }
        // bionic's PTHREAD_ONCE_INIT is 0, and so is glibc's. A control that
        // was only ever statically initialised is valid for both.
        let mut control: c_int = 0;
        // SAFETY: `control` is a live, statically-zeroed `pthread_once_t` on
        // this thread's stack, per `once`'s own contract.
        unsafe {
            assert_eq!(once(&mut control, Some(init)), 0);
            assert_eq!(once(&mut control, Some(init)), 0);
        }
        assert_eq!(RUNS.load(Ordering::SeqCst), 1);

        // The control, not the routine, is what remembers. A second control
        // runs it again — otherwise the count above proves nothing.
        let mut second: c_int = 0;
        // SAFETY: as above.
        unsafe {
            assert_eq!(once(&mut second, Some(init)), 0);
        }
        assert_eq!(RUNS.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn thread_specific_data_round_trips() {
        let mut key: c_int = -1;
        // SAFETY: `key` is a live 4-byte slot on this thread's stack, per
        // `key_create`/`setspecific`'s own contracts.
        unsafe {
            assert_eq!(key_create(&mut key, None), 0);
        }
        // A key with nothing stored reads as null, which is what the caller
        // that used to get a stubbed 0 was entitled to expect.
        assert!(getspecific(key).is_null());
        let value = 0xC0FFEEusize as *const c_void;
        // SAFETY: as above.
        unsafe {
            assert_eq!(setspecific(key, value), 0);
        }
        assert_eq!(getspecific(key), value as *mut c_void);
        assert_eq!(key_delete(key), 0);
    }

    #[test]
    fn thread_specific_data_is_per_thread() {
        // The property bionic's own implementation would have got wrong here:
        // it reads the slot table hanging off the thread pointer, which for a
        // host-created thread is glibc's.
        let mut key: c_int = -1;
        // SAFETY: as `thread_specific_data_round_trips` above.
        unsafe {
            assert_eq!(key_create(&mut key, None), 0);
            assert_eq!(setspecific(key, 1 as *const c_void), 0);
        }

        let elsewhere = key;
        let seen = std::thread::spawn(move || getspecific(elsewhere) as usize)
            .join()
            .unwrap();
        assert_eq!(seen, 0, "a fresh thread must not see this thread's value");
        assert_eq!(getspecific(key) as usize, 1);
        assert_eq!(key_delete(key), 0);
    }

    #[test]
    fn null_arguments_are_refused_rather_than_dereferenced() {
        // SAFETY: both refuse a null argument before doing anything with it,
        // which is exactly what this test asserts.
        unsafe {
            assert_eq!(once(std::ptr::null_mut(), None), libc_einval());
            assert_eq!(key_create(std::ptr::null_mut(), None), libc_einval());
        }
    }

    #[test]
    fn semaphore_counts() {
        let mut storage = [0u64; 2];
        let sem = storage.as_mut_ptr() as *mut c_void;
        assert_eq!(semaphore_init(sem, 0, 1), 0);
        assert_eq!(semaphore_wait(sem), 0); // consumes the one permit
        assert_ne!(semaphore_trywait(sem), 0); // none left
        assert_eq!(semaphore_post(sem), 0);
        assert_eq!(semaphore_trywait(sem), 0);
        assert_eq!(semaphore_destroy(sem), 0);
    }

    // ------------------------------------------------- the safe inner functions
    //
    // The tests above go through the `extern "C"` entry points on storage cast
    // from a `u64` array. These build the wrapper as the Rust value it is and
    // call what the entry points delegate to, so they cover the lazy-init
    // protocol itself rather than the pointer cast in front of it.

    fn fresh_cond() -> BionicCond {
        BionicCond {
            state: AtomicU32::new(0),
            real_lo: AtomicU32::new(0),
            real_hi: AtomicU32::new(0),
            _reserved: [0; 9],
        }
    }

    fn fresh_sem() -> BionicSem {
        BionicSem {
            state: AtomicU32::new(0),
            real_lo: AtomicU32::new(0),
            real_hi: AtomicU32::new(0),
            _reserved: 0,
        }
    }

    fn backing_address(real_lo: &AtomicU32, real_hi: &AtomicU32) -> u64 {
        real_lo.load(Ordering::SeqCst) as u64 | ((real_hi.load(Ordering::SeqCst) as u64) << 32)
    }

    #[test]
    fn first_use_creates_the_backing_object_and_destroy_returns_the_wrapper_to_zero() {
        let c = fresh_cond();
        assert_eq!(on_cond(&c, pthread_cond_signal), 0);
        assert_eq!(c.state.load(Ordering::SeqCst), READY);
        assert_ne!(backing_address(&c.real_lo, &c.real_hi), 0);

        destroy_backing(&c);
        assert_eq!(c.state.load(Ordering::SeqCst), UNINIT);
        assert_eq!(backing_address(&c.real_lo, &c.real_hi), 0);

        // The wrapper is reusable, which is what a bionic condition variable
        // that is destroyed and then used again by sloppy code relies on.
        assert_eq!(on_cond(&c, pthread_cond_broadcast), 0);
        destroy_backing(&c);
        // And destroying the already destroyed is nothing, not a double free.
        destroy_backing(&c);
        assert_eq!(c.state.load(Ordering::SeqCst), UNINIT);
    }

    #[test]
    fn a_wrapper_in_a_state_this_file_never_wrote_is_refused_not_adopted() {
        let c = fresh_cond();
        c.state.store(0xDEAD_BEEF, Ordering::SeqCst);
        assert!(cond_backing(&c).is_none());
        assert_eq!(on_cond(&c, pthread_cond_signal), libc_einval());
        // Refusing must not have scribbled on it: a caller that hands over
        // garbage should see the same garbage when it looks again.
        assert_eq!(c.state.load(Ordering::SeqCst), 0xDEAD_BEEF);
        // An explicit init is the documented way back, and replaces it.
        assert_eq!(cond_init_on(&c, std::ptr::null()), 0);
        assert_eq!(c.state.load(Ordering::SeqCst), READY);
        destroy_backing(&c);
    }

    #[test]
    fn threads_racing_on_a_fresh_wrapper_agree_on_one_backing_object() {
        // Eight threads reach the same all-zero condition variable at once.
        // Exactly one may allocate; the rest must see its address, never their
        // own, or a signal would go to an object nobody waits on.
        let c = std::sync::Arc::new(fresh_cond());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let (c, barrier) = (c.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    cond_backing(&c).expect("a fresh wrapper resolves") as usize
                })
            })
            .collect();
        let addresses: Vec<usize> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        assert!(addresses.windows(2).all(|w| w[0] == w[1]), "{addresses:x?}");
        destroy_backing(&*c);
    }

    #[test]
    fn a_semaphore_wrapper_counts_through_the_inner_functions() {
        let s = fresh_sem();
        assert_eq!(semaphore_init_on(&s, 0, 2), 0);
        assert_eq!(on_sem(&s, sem_wait), 0);
        assert_eq!(on_sem(&s, sem_wait), 0);
        assert_ne!(on_sem(&s, sem_trywait), 0, "two permits, two waits, none left");
        destroy_backing(&s);

        // A semaphore that skipped `sem_init` is created with a zero count
        // rather than crashing: one post makes exactly one permit.
        let skipped = fresh_sem();
        assert_eq!(on_sem(&skipped, sem_post), 0);
        assert_eq!(on_sem(&skipped, sem_trywait), 0);
        assert_ne!(on_sem(&skipped, sem_trywait), 0);
        destroy_backing(&skipped);
    }

    #[test]
    fn re_initialising_a_wrapper_replaces_its_backing_object() {
        let s = fresh_sem();
        assert_eq!(semaphore_init_on(&s, 0, 0), 0);
        assert_ne!(on_sem(&s, sem_trywait), 0);
        // A second init with a count must be a new semaphore, not the old one.
        assert_eq!(semaphore_init_on(&s, 0, 1), 0);
        assert_eq!(on_sem(&s, sem_trywait), 0);
        destroy_backing(&s);
    }

    #[test]
    fn every_entry_point_refuses_a_null_object_with_einval() {
        let null = std::ptr::null_mut::<c_void>();
        let einval = libc_einval();
        let mut storage = [0u64; 6];
        let live = storage.as_mut_ptr() as *mut c_void;
        // SAFETY: each refuses a null first argument before touching it; the
        // timed wait's other pointers are never read on that path.
        unsafe {
            assert_eq!(cond_init(null, std::ptr::null()), einval);
            assert_eq!(cond_wait(null, live), einval);
            assert_eq!(cond_timedwait(null, live, std::ptr::null()), einval);
        }
        assert_eq!(cond_destroy(null), einval);
        assert_eq!(cond_signal(null), einval);
        assert_eq!(cond_broadcast(null), einval);
        assert_eq!(semaphore_init(null, 0, 0), einval);
        assert_eq!(semaphore_destroy(null), einval);
        assert_eq!(semaphore_post(null), einval);
        assert_eq!(semaphore_wait(null), einval);
        assert_eq!(semaphore_trywait(null), einval);

        // A good condition variable with no mutex to release is refused too,
        // and does so before it blocks.
        // SAFETY: `live` is six zeroed words, a valid static-initialised cond.
        unsafe {
            assert_eq!(cond_wait(live, null), einval);
            assert_eq!(cond_timedwait(live, null, std::ptr::null()), einval);
        }
        assert_eq!(cond_destroy(live), 0);
    }

    #[test]
    fn a_once_control_is_borrowed_as_the_atomic_it_is() {
        static RUNS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        unsafe extern "C" fn init() {
            RUNS.fetch_add(1, Ordering::SeqCst);
        }
        let control = AtomicI32::new(0);
        assert_eq!(once_on(&control, Some(init)), 0);
        assert_eq!(once_on(&control, Some(init)), 0);
        assert_eq!(RUNS.load(Ordering::SeqCst), 1);
        assert_ne!(control.load(Ordering::SeqCst), 0, "glibc recorded completion in the control");
    }

    #[test]
    fn a_created_key_is_returned_as_the_value_not_through_a_pointer() {
        let key = new_key(None).expect("the host has keys to spare");
        assert!(getspecific(key).is_null());
        assert_eq!(key_delete(key), 0);
        // Deleting the same key twice is the host's EINVAL, passed through.
        assert_ne!(key_delete(key), 0);
    }

    #[test]
    fn static_mutex_words_map_to_the_kind_they_were_declared_with() {
        assert_eq!(static_mutex_kind(0), Some(StaticMutexKind::Normal));
        assert_eq!(static_mutex_kind(1 << 14), Some(StaticMutexKind::Recursive));
        assert_eq!(static_mutex_kind(2 << 14), Some(StaticMutexKind::ErrorCheck));
        // Our own markers and anything hand-written are not static patterns.
        for raw in [READY, INITIALISING, 3 << 14, 0x1234, u32::MAX] {
            assert_eq!(static_mutex_kind(raw), None, "{raw:#x}");
        }
    }

    /// The numbers `glibc_type` hands to `pthread_mutexattr_settype` are only
    /// right if glibc agrees, and the only evidence that it does is behaviour:
    /// a recursive mutex lets its owner lock again, an error-checking one
    /// reports EDEADLK instead.
    #[test]
    fn the_glibc_mutex_types_behave_as_their_names_say() {
        unsafe extern "C" {
            fn pthread_mutexattr_init(attr: *mut c_void) -> c_int;
            fn pthread_mutexattr_destroy(attr: *mut c_void) -> c_int;
            fn pthread_mutexattr_settype(attr: *mut c_void, kind: c_int) -> c_int;
            fn pthread_mutex_init(m: *mut c_void, attr: *const c_void) -> c_int;
            fn pthread_mutex_destroy(m: *mut c_void) -> c_int;
            fn pthread_mutex_lock(m: *mut c_void) -> c_int;
            fn pthread_mutex_trylock(m: *mut c_void) -> c_int;
            fn pthread_mutex_unlock(m: *mut c_void) -> c_int;
        }
        assert_eq!(StaticMutexKind::Normal.glibc_type(), None);
        for (kind, relock) in [(StaticMutexKind::Recursive, 0), (StaticMutexKind::ErrorCheck, 35)] {
            let glibc_kind = kind.glibc_type().expect("not the default");
            let (mut attr, mut mutex) = ([0u64; 16], [0u64; 16]);
            let (a, m) = (attr.as_mut_ptr() as *mut c_void, mutex.as_mut_ptr() as *mut c_void);
            // SAFETY: both are 128 zeroed, 8-aligned bytes, far larger than
            // glibc's attribute and mutex objects, used on this thread only.
            unsafe {
                assert_eq!(pthread_mutexattr_init(a), 0);
                assert_eq!(pthread_mutexattr_settype(a, glibc_kind), 0);
                assert_eq!(pthread_mutex_init(m, a), 0);
                assert_eq!(pthread_mutexattr_destroy(a), 0);
                assert_eq!(pthread_mutex_lock(m), 0);
                // `relock` is what a second lock by the owner answers: success
                // for recursive, EDEADLK (35) for error-checking.
                let rc = pthread_mutex_trylock(m);
                if relock == 0 {
                    assert_eq!(rc, 0, "{kind:?}");
                    assert_eq!(pthread_mutex_unlock(m), 0);
                } else {
                    assert_eq!(rc, 16, "{kind:?}: trylock on a held error-checking mutex is EBUSY");
                    assert_eq!(pthread_mutex_lock(m), relock, "{kind:?}");
                }
                assert_eq!(pthread_mutex_unlock(m), 0);
                assert_eq!(pthread_mutex_destroy(m), 0);
            }
        }
    }

    #[test]
    fn mutex_words_are_reported_eight_hex_digits_each() {
        // The three words seen on the Quest build's abort, quoted in
        // `wait_mutex`'s comment.
        assert_eq!(format_words(&[2, 0xfc00_1e00, 0x0002_0754]), "0x00000002 0xfc001e00 0x00020754");
        assert_eq!(format_words(&[]), "");
    }

    // ---------------------------------------------------- aarch64-only: mutexes

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn statically_initialised_normal_mutex_locks_and_unlocks() {
        // bionic's PTHREAD_MUTEX_INITIALIZER is all zeroes; locking one that
        // was never explicitly initialised must still work, and must behave
        // as a normal (non-recursive) mutex.
        let mut storage = [0u64; 5]; // 40 bytes
        let m = storage.as_mut_ptr() as *mut c_void;
        assert_eq!(mutex_lock(m), 0);
        assert_eq!(mutex_unlock(m), 0);
        assert_eq!(mutex_destroy(m), 0);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn statically_initialised_recursive_mutex_can_be_relocked_by_its_owner() {
        // PTHREAD_RECURSIVE_MUTEX_INITIALIZER_NP's raw value:
        // (PTHREAD_MUTEX_RECURSIVE & 3) << MUTEX_TYPE_SHIFT, i.e. 1 << 14 --
        // see `static_mutex_kind`'s own comment for where these numbers come
        // from. A mutex declared with this macro and never passed to
        // `pthread_mutex_init` must still behave as recursive on its very
        // first lock.
        const RECURSIVE_INITIALIZER: u64 = 1 << 14;
        let mut storage = [0u64; 5];
        storage[0] = RECURSIVE_INITIALIZER;
        let m = storage.as_mut_ptr() as *mut c_void;
        assert_eq!(mutex_lock(m), 0);
        assert_eq!(
            mutex_trylock(m),
            0,
            "a recursive mutex must let its own owner lock it again"
        );
        assert_eq!(mutex_unlock(m), 0);
        assert_eq!(mutex_unlock(m), 0);
        assert_eq!(mutex_destroy(m), 0);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn statically_initialised_errorcheck_mutex_refuses_a_self_relock() {
        // PTHREAD_ERRORCHECK_MUTEX_INITIALIZER_NP: (PTHREAD_MUTEX_ERRORCHECK
        // & 3) << 14, i.e. 2 << 14. The opposite property from the recursive
        // case above -- proof that the two static patterns are not being
        // confused for one another.
        const ERRORCHECK_INITIALIZER: u64 = 2 << 14;
        let mut storage = [0u64; 5];
        storage[0] = ERRORCHECK_INITIALIZER;
        let m = storage.as_mut_ptr() as *mut c_void;
        assert_eq!(mutex_lock(m), 0);
        assert_ne!(
            mutex_lock(m),
            0,
            "an errorcheck mutex must refuse a self-relock rather than deadlock or succeed"
        );
        assert_eq!(mutex_unlock(m), 0);
        assert_eq!(mutex_destroy(m), 0);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn mutex_lock_is_exclusive_under_contention() {
        // If the lock were not truly exclusive -- the whole point of the
        // wrapper existing -- concurrent threads racing on this
        // load-then-store would lose increments and the final count would
        // come out short.
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        const THREADS: usize = 8;
        const ITERS: usize = 2000;

        let mut storage = [0u64; 5];
        let m = storage.as_mut_ptr() as *mut c_void;
        assert_eq!(mutex_init(m, std::ptr::null()), 0);
        let address = m as usize;

        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                std::thread::spawn(move || {
                    let mutex = address as *mut c_void;
                    for _ in 0..ITERS {
                        assert_eq!(mutex_lock(mutex), 0);
                        let v = COUNTER.load(Ordering::Relaxed);
                        COUNTER.store(v + 1, Ordering::Relaxed);
                        assert_eq!(mutex_unlock(mutex), 0);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(COUNTER.load(Ordering::SeqCst), (THREADS * ITERS) as u32);
        assert_eq!(mutex_destroy(m), 0);
    }

    #[cfg(target_arch = "aarch64")]
    fn fresh_mutex(state: u32) -> BionicMutex {
        BionicMutex {
            state: AtomicU32::new(state),
            real_lo: AtomicU32::new(0),
            real_hi: AtomicU32::new(0),
            _reserved: [0; 7],
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn the_mutex_inner_function_refuses_a_null_or_corrupt_mutex() {
        let einval = libc_einval();
        let null = std::ptr::null_mut::<c_void>();
        assert_eq!(on_mutex(None, null, "pthread_mutex_lock", pthread_mutex_lock), einval);

        let corrupt = fresh_mutex(0x1234);
        assert_eq!(on_mutex(Some(&corrupt), null, "pthread_mutex_lock", pthread_mutex_lock), einval);
        assert_eq!(corrupt.state.load(Ordering::SeqCst), 0x1234, "refused, not adopted");
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn a_static_recursive_pattern_resolves_to_a_recursive_backing_mutex() {
        let m = fresh_mutex(1 << 14);
        let null = std::ptr::null_mut::<c_void>();
        assert_eq!(on_mutex(Some(&m), null, "pthread_mutex_lock", pthread_mutex_lock), 0);
        assert_eq!(m.state.load(Ordering::SeqCst), READY);
        assert_eq!(on_mutex(Some(&m), null, "pthread_mutex_trylock", pthread_mutex_trylock), 0);
        assert_eq!(on_mutex(Some(&m), null, "pthread_mutex_unlock", pthread_mutex_unlock), 0);
        assert_eq!(on_mutex(Some(&m), null, "pthread_mutex_unlock", pthread_mutex_unlock), 0);
        destroy_backing(&m);
        assert_eq!(m.state.load(Ordering::SeqCst), UNINIT);
    }

    // ------------------------------------------------------- aarch64-only: attrs

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn attr_setstacksize_round_trips_through_the_getter() {
        let mut storage = [0u64; 7]; // 56 bytes
        let attr = storage.as_mut_ptr() as *mut c_void;
        assert_eq!(attr_init(attr), 0);
        let requested: usize = 1 << 20;
        assert_eq!(attr_setstacksize(attr, requested), 0);
        let mut got: usize = 0;
        assert_eq!(attr_getstacksize(attr, &mut got), 0);
        assert_eq!(got, requested);
        assert_eq!(attr_destroy(attr), 0);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn attr_round_trips_a_set_stack_size_through_pthread_create() {
        // Exercises the real path an engine call takes: this file's
        // `attr_init`/`attr_setstacksize`, then `native/thread_trace.cpp`'s
        // `cordial_pthread_create` (which calls back into
        // `cordial_pthread_attr_real` on aarch64 before forwarding to the
        // host's own `pthread_create`), then the new thread reading its own
        // stack size back via `getattr_np`/`attr_getstacksize` -- the same
        // two functions Roblox's engine would call, on the same object,
        // through the same wrapper.
        unsafe extern "C" {
            fn cordial_pthread_create(
                thread: *mut c_ulong,
                attr: *const c_void,
                start_routine: extern "C" fn(*mut c_void) -> *mut c_void,
                arg: *mut c_void,
            ) -> c_int;
            fn pthread_join(thread: c_ulong, retval: *mut *mut c_void) -> c_int;
            fn pthread_self() -> c_ulong;
        }

        static SEEN_STACK: AtomicU32 = AtomicU32::new(0);
        // A whole page above PTHREAD_STACK_MIN on any Linux, and page-aligned
        // so nothing rounds it before the read-back.
        const REQUESTED_STACK: usize = 1 << 20;

        unsafe extern "C" fn start(_: *mut c_void) -> *mut c_void {
            let mut self_attr_storage = [0u64; 7];
            let self_attr = self_attr_storage.as_mut_ptr() as *mut c_void;
            // SAFETY: `pthread_self` takes no arguments and returns the
            // calling thread's own handle.
            let me = unsafe { pthread_self() };
            assert_eq!(getattr_np(me, self_attr), 0);
            let mut size: usize = 0;
            assert_eq!(attr_getstacksize(self_attr, &mut size), 0);
            SEEN_STACK.store(size as u32, Ordering::SeqCst);
            assert_eq!(attr_destroy(self_attr), 0);
            std::ptr::null_mut()
        }

        let mut attr_storage = [0u64; 7];
        let attr = attr_storage.as_mut_ptr() as *mut c_void;
        assert_eq!(attr_init(attr), 0);
        assert_eq!(attr_setstacksize(attr, REQUESTED_STACK), 0);

        let mut thread: c_ulong = 0;
        // SAFETY: `attr` is a live, initialised BionicAttr; `start` matches
        // the expected signature and returns null.
        let rc = unsafe {
            cordial_pthread_create(&mut thread, attr, start, std::ptr::null_mut())
        };
        assert_eq!(rc, 0, "pthread_create with a translated attr must succeed");
        // SAFETY: `thread` came from the `cordial_pthread_create` call above.
        assert_eq!(unsafe { pthread_join(thread, std::ptr::null_mut()) }, 0);

        // At least, not exactly: glibc may round a requested stack up, and on
        // Fedora 44 aarch64 a 1 MiB request came back as 2 MiB and failed the
        // v0.21.0 RPM build. What matters is that the attr's size reached the
        // thread rather than being dropped for the default, which is 8 MiB or
        // more, so the bound above keeps that failure visible.
        let seen = SEEN_STACK.load(Ordering::SeqCst) as usize;
        assert!(
            seen >= REQUESTED_STACK && seen <= 4 * REQUESTED_STACK,
            "the new thread must see at least the stack size that was set on \
             the attr it was created with, round-tripped through \
             pthread_create and pthread_getattr_np (saw {seen})"
        );
        assert_eq!(attr_destroy(attr), 0);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn a_null_attr_is_refused_and_left_alone_by_pthread_create() {
        let null = std::ptr::null_mut::<c_void>();
        assert_eq!(attr_init(null), libc_einval());
        assert_eq!(attr_destroy(null), libc_einval());
        assert_eq!(attr_setstacksize(null, 1 << 20), libc_einval());
        assert_eq!(getattr_np(0, null), libc_einval());
        // `pthread_create` passes null for "default attributes", and the
        // translation must hand that back as it is.
        assert!(cordial_pthread_attr_real(std::ptr::null()).is_null());
    }
}
