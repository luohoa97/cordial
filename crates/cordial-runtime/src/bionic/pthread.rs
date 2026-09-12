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
//! declaring `char sz_x[sizeof(x)];` per type, compiled twice and the symbol
//! sizes read back with `nm -S`: once against this tree's
//! `third_party/mcpelauncher-linker/bionic/libc/include` at
//! `-target x86_64-linux-android -D__ANDROID_API__=33`, once against the host's
//! glibc (2.43 when this was written).
//!
//! | type                  | bionic | glibc | |
//! |-----------------------|-------:|------:|-|
//! | `pthread_mutex_t`     |     40 |    40 | passthrough |
//! | `pthread_rwlock_t`    |     56 |    56 | passthrough |
//! | `pthread_attr_t`      |     56 |    56 | passthrough — layouts differ, but Roblox only ever hands the struct back to us |
//! | `pthread_once_t`      |      4 |     4 | forwarded |
//! | `pthread_key_t`       |      4 |     4 | forwarded |
//! | `pthread_cond_t`      |     48 |    48 | wrapped, on a size that was wrong — see below |
//! | **`sem_t`**           | **16** |**32** | **wrapped** |
//!
//! `sem_t` is a real mismatch: glibc's `sem_init` writes 32 bytes into a
//! 16-byte object, so the 16 bytes after it belong to whatever was adjacent.
//! The wrapper is what stops that.
//!
//! **The `pthread_cond_t` row used to read 32 against 48, and it was wrong.**
//! 32 bytes is `pthread_barrier_t`, which is `int64_t __private[4]`;
//! `pthread_cond_t` is `int32_t __private[12]`, a few declarations further down
//! the same header, and comes to 48 on LP64 — the same as glibc's. The commit
//! that introduced this module recorded the overrun as one of three ABI
//! divergences found, and the measurement above says there was no overrun to
//! find. The wrapper stays for now: it is harmless either way, since it only
//! ever writes the first 16 bytes of the caller's object, and taking it out
//! changes what runs at every `pthread_cond_wait` in the engine — a behaviour
//! change that wants its own measurement rather than a free ride on this one.
//!
//! Initialisation of a wrapper is lazy because bionic's
//! `PTHREAD_COND_INITIALIZER` is all zeroes, so a statically-initialised
//! condition variable can reach `wait` without `init` ever being called.

use std::ffi::{c_int, c_uint, c_void};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Marks a wrapper whose backing object exists. Arbitrary, but distinctive in a
/// memory dump and impossible to reach by zero-initialisation.
const READY: u64 = 0xC0D1A1_C0FFEE;

const UNINIT: u64 = 0;
const INITIALISING: u64 = 1;

/// An overlay on the first 32 bytes of bionic's 48-byte `pthread_cond_t`. The
/// remaining 16 are never touched; only `state` and `real` are ours.
#[repr(C)]
struct BionicCond {
    state: AtomicU64,
    real: AtomicUsize,
    _reserved: [u64; 2],
}

/// bionic's `sem_t`: `unsigned int count; int __reserved[3];`
#[repr(C)]
struct BionicSem {
    state: AtomicU64,
    real: AtomicUsize,
}

// glibc's implementations. These resolve to the host's libc at link time; our
// own wrappers are never exported, so there is no recursion.
extern "C" {
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
    drop(Box::from_raw(ptr as *mut [u8; BACKING_SIZE]));
}

/// Resolve the real object behind a wrapper, creating it on first use.
///
/// `init` is called exactly once, with the freshly allocated backing store.
///
/// SAFETY: `state` and `real` must belong to the same live wrapper object.
unsafe fn resolve(
    state: &AtomicU64,
    real: &AtomicUsize,
    init: impl FnOnce(*mut c_void),
) -> *mut c_void {
    loop {
        match state.compare_exchange(UNINIT, INITIALISING, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => {
                let backing = alloc_backing();
                init(backing);
                real.store(backing as usize, Ordering::Release);
                state.store(READY, Ordering::Release);
                return backing;
            }
            Err(READY) => return real.load(Ordering::Acquire) as *mut c_void,
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

// ---------------------------------------------------------------- condition vars

pub extern "C" fn cond_init(cond: *mut c_void, attr: *const c_void) -> c_int {
    if cond.is_null() {
        return libc_einval();
    }
    // SAFETY: bionic's contract is a pointer to a 32-byte pthread_cond_t.
    let c = unsafe { &mut *(cond as *mut BionicCond) };
    // An explicit init on an object we already wrapped replaces it, matching
    // glibc's "undefined behaviour, but do something sane" posture.
    unsafe { destroy_backing(&c.state, &c.real, pthread_cond_destroy) };
    c.state.store(UNINIT, Ordering::Release);
    let backing = unsafe {
        resolve(&c.state, &c.real, |p| {
            pthread_cond_init(p, attr);
        })
    };
    if backing.is_null() {
        libc_einval()
    } else {
        0
    }
}

pub extern "C" fn cond_destroy(cond: *mut c_void) -> c_int {
    if cond.is_null() {
        return libc_einval();
    }
    // SAFETY: as above.
    let c = unsafe { &mut *(cond as *mut BionicCond) };
    unsafe { destroy_backing(&c.state, &c.real, pthread_cond_destroy) };
    0
}

/// Tear down and free a wrapper's backing object, if it has one.
///
/// SAFETY: `state`/`real` must belong to the same live wrapper.
unsafe fn destroy_backing(
    state: &AtomicU64,
    real: &AtomicUsize,
    destroy: unsafe extern "C" fn(*mut c_void) -> c_int,
) {
    if state.swap(UNINIT, Ordering::AcqRel) == READY {
        let p = real.swap(0, Ordering::AcqRel) as *mut c_void;
        if !p.is_null() {
            destroy(p);
            free_backing(p);
        }
    }
}

macro_rules! cond_op {
    ($name:ident, $glibc:ident) => {
        pub extern "C" fn $name(cond: *mut c_void) -> c_int {
            if cond.is_null() {
                return libc_einval();
            }
            // SAFETY: bionic's contract is a pointer to a 32-byte pthread_cond_t.
            let c = unsafe { &mut *(cond as *mut BionicCond) };
            let backing = unsafe {
                resolve(&c.state, &c.real, |p| {
                    pthread_cond_init(p, std::ptr::null());
                })
            };
            if backing.is_null() {
                return libc_einval();
            }
            unsafe { $glibc(backing) }
        }
    };
}

cond_op!(cond_signal, pthread_cond_signal);
cond_op!(cond_broadcast, pthread_cond_broadcast);

pub extern "C" fn cond_wait(cond: *mut c_void, mutex: *mut c_void) -> c_int {
    let Some(backing) = cond_backing(cond) else {
        return libc_einval();
    };
    // On FreeBSD the engine's bionic mutexes are backed by a side-table of real
    // FreeBSD mutexes (bionic's layout is not FreeBSD's), so pthread_cond_wait
    // must operate on that same real mutex — the one mutex_lock/unlock use — not
    // the bionic object. Handing it the bionic bytes makes the wait never truly
    // block or signal, which livelocks the engine.
    #[cfg(target_os = "freebsd")]
    let mutex = unsafe {
        extern "C" {
            fn bionic_mutex_real(m: *mut c_void) -> *mut c_void;
        }
        bionic_mutex_real(mutex)
    };
    // SAFETY: `mutex` is now a real host mutex (FreeBSD) or the bionic/glibc
    // pthread_mutex_t which is layout-identical on x86-64 (Linux).
    unsafe { pthread_cond_wait(backing, mutex) }
}

pub extern "C" fn cond_timedwait(
    cond: *mut c_void,
    mutex: *mut c_void,
    abstime: *const c_void,
) -> c_int {
    let Some(backing) = cond_backing(cond) else {
        return libc_einval();
    };
    // Same bionic-mutex -> real-mutex translation as cond_wait (see there).
    #[cfg(target_os = "freebsd")]
    let mutex = unsafe {
        extern "C" {
            fn bionic_mutex_real(m: *mut c_void) -> *mut c_void;
        }
        bionic_mutex_real(mutex)
    };
    // SAFETY: as above; `struct timespec` is identical between the two libcs.
    unsafe { pthread_cond_timedwait(backing, mutex, abstime) }
}

fn cond_backing(cond: *mut c_void) -> Option<*mut c_void> {
    if cond.is_null() {
        return None;
    }
    // SAFETY: bionic's contract is a pointer to a 32-byte pthread_cond_t.
    let c = unsafe { &mut *(cond as *mut BionicCond) };
    let backing = unsafe {
        resolve(&c.state, &c.real, |p| {
            pthread_cond_init(p, std::ptr::null());
        })
    };
    (!backing.is_null()).then_some(backing)
}

// ------------------------------------------------------------------ semaphores

pub extern "C" fn semaphore_init(sem: *mut c_void, pshared: c_int, value: u32) -> c_int {
    if sem.is_null() {
        return libc_einval();
    }
    // SAFETY: bionic's contract is a pointer to a 16-byte sem_t.
    let s = unsafe { &mut *(sem as *mut BionicSem) };
    unsafe { destroy_backing(&s.state, &s.real, sem_destroy) };
    s.state.store(UNINIT, Ordering::Release);
    let backing = unsafe {
        resolve(&s.state, &s.real, |p| {
            sem_init(p, pshared, value);
        })
    };
    if backing.is_null() {
        libc_einval()
    } else {
        0
    }
}

pub extern "C" fn semaphore_destroy(sem: *mut c_void) -> c_int {
    if sem.is_null() {
        return libc_einval();
    }
    // SAFETY: as above.
    let s = unsafe { &mut *(sem as *mut BionicSem) };
    unsafe { destroy_backing(&s.state, &s.real, sem_destroy) };
    0
}

macro_rules! sem_op {
    ($name:ident, $glibc:ident) => {
        pub extern "C" fn $name(sem: *mut c_void) -> c_int {
            if sem.is_null() {
                return libc_einval();
            }
            // SAFETY: bionic's contract is a pointer to a 16-byte sem_t.
            let s = unsafe { &mut *(sem as *mut BionicSem) };
            // Unlike condition variables a semaphore has no static initialiser,
            // so reaching here uninitialised means sem_init was skipped. Create
            // a zero-count semaphore rather than crashing.
            let backing = unsafe {
                resolve(&s.state, &s.real, |p| {
                    sem_init(p, 0, 0);
                })
            };
            if backing.is_null() {
                return libc_einval();
            }
            unsafe { $glibc(backing) }
        }
    };
}

sem_op!(semaphore_post, sem_post);
sem_op!(semaphore_wait, sem_wait);
sem_op!(semaphore_trywait, sem_trywait);

// ------------------------------------------------- once, and thread-local keys

// glibc's. Renamed so the forwarding wrappers below can carry bionic's names.
extern "C" {
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
pub extern "C" fn once(control: *mut c_int, init_routine: Option<extern "C" fn()>) -> c_int {
    if control.is_null() {
        return libc_einval();
    }
    // FreeBSD's `pthread_once_t` is a struct (int state + mutex pointer) with
    // different init semantics than bionic's / glibc's 4-byte atomic int:
    // forwarding bionic's zero-initialised control makes FreeBSD's pthread_once
    // treat it as already-run and SKIP the init routine. That silently breaks
    // every lazily-initialised subsystem (memory-category tracking, TSD keys).
    // Run the once protocol ourselves on the 4-byte slot instead (2 = done).
    #[cfg(target_os = "freebsd")]
    {
        use core::sync::atomic::{AtomicI32, Ordering};
        static GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());
        // SAFETY: `control` is a live 4-byte slot; we only ever CAS it.
        let ctrl = unsafe { &*(control as *const AtomicI32) };
        if ctrl.load(Ordering::Acquire) == 2 {
            return 0;
        }
        let _g = GUARD.lock().unwrap_or_else(|e| e.into_inner());
        if ctrl.load(Ordering::Relaxed) != 2 {
            if let Some(f) = init_routine {
                f();
            }
            ctrl.store(2, Ordering::Release);
        }
        return 0;
    }
    #[cfg(not(target_os = "freebsd"))]
    // SAFETY: `control` points at 4 bytes in both libcs, and a bionic
    // once-control that has never been passed to bionic's implementation holds
    // a state glibc's understands. `init_routine` is a plain `void (*)(void)`.
    unsafe { host_pthread_once(control, init_routine) }
}

/// `pthread_key_create`. bionic's `pthread_key_t` is signed, glibc's is not,
/// hence the local rather than a cast of the caller's pointer — and nothing is
/// written back unless the host says it succeeded, which is bionic's contract.
pub extern "C" fn key_create(
    key: *mut c_int,
    destructor: Option<extern "C" fn(*mut c_void)>,
) -> c_int {
    if key.is_null() {
        return libc_einval();
    }
    let mut host_key: c_uint = 0;
    // SAFETY: `host_key` is a live 4-byte slot; the destructor is passed through
    // untouched and glibc calls it on the same thread bionic would have.
    let rc = unsafe { host_pthread_key_create(&mut host_key, destructor) };
    if rc == 0 {
        // SAFETY: the caller's `pthread_key_t*`, 4 bytes in both libcs.
        unsafe { *key = host_key as c_int };
    }
    rc
}

pub extern "C" fn key_delete(key: c_int) -> c_int {
    // SAFETY: a key is an opaque scalar; an invalid one is rejected by glibc.
    unsafe { host_pthread_key_delete(key as c_uint) }
}

pub extern "C" fn getspecific(key: c_int) -> *mut c_void {
    // SAFETY: as above. A key never created returns null, as bionic's does.
    unsafe { host_pthread_getspecific(key as c_uint) }
}

pub extern "C" fn setspecific(key: c_int, value: *const c_void) -> c_int {
    // SAFETY: as above. `value` is stored, never dereferenced.
    unsafe { host_pthread_setspecific(key as c_uint, value) }
}

fn libc_einval() -> c_int {
    22 // EINVAL, identical in both libcs
}

/// Everything this module replaces.
pub fn overrides() -> Vec<(&'static str, *mut c_void)> {
    macro_rules! f {
        ($name:literal, $fn:expr) => {
            ($name, $fn as *const () as *mut c_void)
        };
    }
    vec![
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
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrappers_fit_inside_the_bionic_objects() {
        // A wrapper is an overlay on storage the caller allocated to bionic's
        // size, so it must never be larger than bionic's type. `sem_t` is 16
        // and the overlay uses all of it; `pthread_cond_t` is 48 and the
        // overlay uses the first 32.
        assert!(std::mem::size_of::<BionicCond>() <= 48);
        assert_eq!(std::mem::size_of::<BionicSem>(), 16);
    }

    #[test]
    fn statically_initialised_cond_works() {
        // bionic's PTHREAD_COND_INITIALIZER is all zeroes; signalling one that
        // was never explicitly initialised must still work.
        // u64, not u8: the wrapper's first field is an AtomicU64, so byte
        // storage is under-aligned and the cast is UB. Real conds come from the
        // engine's own allocations and are always aligned.
        let mut storage = [0u64; 4];
        let cond = storage.as_mut_ptr() as *mut c_void;
        assert_eq!(cond_signal(cond), 0);
        assert_eq!(cond_broadcast(cond), 0);
        assert_eq!(cond_destroy(cond), 0);
    }

    #[test]
    fn init_destroy_roundtrip_does_not_leak_state() {
        let mut storage = [0u64; 4];
        let cond = storage.as_mut_ptr() as *mut c_void;
        assert_eq!(cond_init(cond, std::ptr::null()), 0);
        assert_eq!(cond_destroy(cond), 0);
        // Destroyed wrappers return to the zero state, so they can be reused.
        assert_eq!(cond_init(cond, std::ptr::null()), 0);
        assert_eq!(cond_destroy(cond), 0);
    }

    #[test]
    fn once_runs_the_initialiser_exactly_once() {
        static RUNS: AtomicU64 = AtomicU64::new(0);
        extern "C" fn init() {
            RUNS.fetch_add(1, Ordering::SeqCst);
        }
        // bionic's PTHREAD_ONCE_INIT is 0, and so is glibc's. A control that
        // was only ever statically initialised is valid for both.
        let mut control: c_int = 0;
        assert_eq!(once(&mut control, Some(init)), 0);
        assert_eq!(once(&mut control, Some(init)), 0);
        assert_eq!(RUNS.load(Ordering::SeqCst), 1);

        // The control, not the routine, is what remembers. A second control
        // runs it again — otherwise the count above proves nothing.
        let mut second: c_int = 0;
        assert_eq!(once(&mut second, Some(init)), 0);
        assert_eq!(RUNS.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn thread_specific_data_round_trips() {
        let mut key: c_int = -1;
        assert_eq!(key_create(&mut key, None), 0);
        // A key with nothing stored reads as null, which is what the caller
        // that used to get a stubbed 0 was entitled to expect.
        assert!(getspecific(key).is_null());
        let value = 0xC0FFEEusize as *const c_void;
        assert_eq!(setspecific(key, value), 0);
        assert_eq!(getspecific(key), value as *mut c_void);
        assert_eq!(key_delete(key), 0);
    }

    #[test]
    fn thread_specific_data_is_per_thread() {
        // The property bionic's own implementation would have got wrong here:
        // it reads the slot table hanging off the thread pointer, which for a
        // host-created thread is glibc's.
        let mut key: c_int = -1;
        assert_eq!(key_create(&mut key, None), 0);
        assert_eq!(setspecific(key, 1 as *const c_void), 0);

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
        assert_eq!(once(std::ptr::null_mut(), None), libc_einval());
        assert_eq!(key_create(std::ptr::null_mut(), None), libc_einval());
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
}
