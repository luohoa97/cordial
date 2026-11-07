//! Signal types, which bionic and glibc disagree about more violently than
//! anything else in this shim.
//!
//! | type              | bionic (LP64) | glibc | |
//! |-------------------|--------------:|------:|-|
//! | `sigset_t`        |    **8** bytes | **128** bytes | a plain `unsigned long` bitmask vs an opaque array |
//! | `struct sigaction`|   **32** bytes | **152** bytes | and the fields are in a different *order* |
//!
//! Roblox imports `sigemptyset`, `sigfillset`, `sigaddset`, `sigaction`,
//! `sigprocmask` and `pthread_sigmask`. Passed straight through, every one of
//! them writes 120 bytes past the end of the caller's object — usually a stack
//! local, since masks almost always are. `sigaction` is worse than an overrun:
//! the field order differs, so glibc reads the handler out of what bionic put
//! the flags in.
//!
//! Nothing about this failure is localised. It corrupts whatever happened to be
//! adjacent, and the damage surfaces later somewhere unrelated — which is the
//! shape of every bug this shim has had to chase.
//!
//! bionic's mask is the kernel's: bit `n-1` means signal `n`. glibc's is opaque,
//! so conversion goes through its own `sigaddset`/`sigismember` rather than
//! assuming a layout.

use std::ffi::{c_int, c_void};

/// bionic's `sigset_t` on LP64.
type BionicSigset = u64;

/// Enough room for glibc's `sigset_t` (128 bytes) with margin.
const GLIBC_SIGSET_BYTES: usize = 192;
/// Enough room for glibc's `struct sigaction` (152 bytes) with margin.
const GLIBC_SIGACTION_BYTES: usize = 256;

/// The highest signal number worth translating. Linux uses 1..=64.
const MAX_SIGNAL: c_int = 64;

/// bionic's `struct sigaction` on LP64. Field order is bionic's, not glibc's.
#[repr(C)]
struct BionicSigaction {
    sa_flags: c_int,
    _pad: c_int,
    sa_handler: *mut c_void,
    sa_mask: BionicSigset,
    sa_restorer: *mut c_void,
}

unsafe extern "C" {
    // glibc's, operating on its own 128-byte sets.
    fn sigemptyset(set: *mut c_void) -> c_int;
    fn sigaddset(set: *mut c_void, sig: c_int) -> c_int;
    fn sigismember(set: *const c_void, sig: c_int) -> c_int;
    fn sigprocmask(how: c_int, set: *const c_void, old: *mut c_void) -> c_int;
    fn pthread_sigmask(how: c_int, set: *const c_void, old: *mut c_void) -> c_int;
    fn sigaction(sig: c_int, act: *const c_void, old: *mut c_void) -> c_int;
}

/// Build a glibc set from bionic's bitmask.
fn to_glibc(mask: BionicSigset, out: &mut [u8; GLIBC_SIGSET_BYTES]) {
    let p = out.as_mut_ptr() as *mut c_void;
    // SAFETY: `out` is at least as large as glibc's sigset_t.
    unsafe { sigemptyset(p) };
    for sig in 1..=MAX_SIGNAL {
        if mask & (1u64 << (sig - 1)) != 0 {
            // SAFETY: `p` is an initialised glibc set; `sig` is in range.
            unsafe { sigaddset(p, sig) };
        }
    }
}

/// Read a glibc set back into bionic's bitmask.
fn from_glibc(set: &[u8; GLIBC_SIGSET_BYTES]) -> BionicSigset {
    let p = set.as_ptr() as *const c_void;
    let mut mask = 0u64;
    for sig in 1..=MAX_SIGNAL {
        // SAFETY: `p` is a glibc set written by one of its own functions.
        if unsafe { sigismember(p, sig) } == 1 {
            mask |= 1u64 << (sig - 1);
        }
    }
    mask
}

// ---------------------------------------------------------------- set builders

unsafe extern "C" fn b_sigemptyset(set: *mut BionicSigset) -> c_int {
    if set.is_null() {
        return -1;
    }
    // SAFETY: bionic's contract is a pointer to its own 8-byte sigset_t. Writing
    // 8 bytes rather than glibc's 128 is the entire point of this wrapper.
    unsafe { *set = 0 };
    0
}

unsafe extern "C" fn b_sigfillset(set: *mut BionicSigset) -> c_int {
    if set.is_null() {
        return -1;
    }
    // SAFETY: as above.
    unsafe { *set = !0 };
    0
}

unsafe extern "C" fn b_sigaddset(set: *mut BionicSigset, sig: c_int) -> c_int {
    if set.is_null() || sig < 1 || sig > MAX_SIGNAL {
        return -1;
    }
    // SAFETY: as above.
    unsafe { *set |= 1u64 << (sig - 1) };
    0
}

unsafe extern "C" fn b_sigdelset(set: *mut BionicSigset, sig: c_int) -> c_int {
    if set.is_null() || sig < 1 || sig > MAX_SIGNAL {
        return -1;
    }
    // SAFETY: as above.
    unsafe { *set &= !(1u64 << (sig - 1)) };
    0
}

unsafe extern "C" fn b_sigismember(set: *const BionicSigset, sig: c_int) -> c_int {
    if set.is_null() || sig < 1 || sig > MAX_SIGNAL {
        return -1;
    }
    // SAFETY: as above.
    let mask = unsafe { *set };
    ((mask >> (sig - 1)) & 1) as c_int
}

// ------------------------------------------------------------------- masking

fn mask_call(
    how: c_int,
    set: *const BionicSigset,
    old: *mut BionicSigset,
    f: unsafe extern "C" fn(c_int, *const c_void, *mut c_void) -> c_int,
) -> c_int {
    let mut new_buf = [0u8; GLIBC_SIGSET_BYTES];
    let mut old_buf = [0u8; GLIBC_SIGSET_BYTES];

    let new_ptr = if set.is_null() {
        std::ptr::null()
    } else {
        // SAFETY: bionic's contract is a pointer to its 8-byte sigset_t.
        to_glibc(unsafe { *set }, &mut new_buf);
        new_buf.as_ptr() as *const c_void
    };
    let old_ptr = if old.is_null() {
        std::ptr::null_mut()
    } else {
        old_buf.as_mut_ptr() as *mut c_void
    };

    // SAFETY: both buffers are large enough for glibc's sigset_t.
    let rc = unsafe { f(how, new_ptr, old_ptr) };

    if !old.is_null() && rc == 0 {
        // SAFETY: glibc wrote the previous mask into `old_buf`.
        unsafe { *old = from_glibc(&old_buf) };
    }
    rc
}

unsafe extern "C" fn b_sigprocmask(
    how: c_int,
    set: *const BionicSigset,
    old: *mut BionicSigset,
) -> c_int {
    mask_call(how, set, old, sigprocmask)
}

unsafe extern "C" fn b_pthread_sigmask(
    how: c_int,
    set: *const BionicSigset,
    old: *mut BionicSigset,
) -> c_int {
    mask_call(how, set, old, pthread_sigmask)
}

// ------------------------------------------------------------------ sigaction

/// glibc's `struct sigaction`, by offset rather than by name.
///
/// Written through raw offsets on purpose: the layout is
/// `{ handler; sigset_t mask; int flags; restorer }`, which is *not* bionic's
/// order, and naming the fields in Rust would only invite someone to reorder
/// them to match the bionic struct above.
const GLIBC_SA_HANDLER: usize = 0;
const GLIBC_SA_MASK: usize = 8;
const GLIBC_SA_FLAGS: usize = 136;
const GLIBC_SA_RESTORER: usize = 144;

/// bionic's `struct sigaction` as glibc's, laid out at glibc's offsets.
///
/// No pointer in sight: the layout is plain bytes at named offsets, so the
/// translation is ordinary Rust that a test can drive and compare against the
/// real `libc::sigaction`.
fn sigaction_to_glibc(a: &BionicSigaction) -> [u8; GLIBC_SIGACTION_BYTES] {
    let mut buf = [0u8; GLIBC_SIGACTION_BYTES];
    let mut mask = [0u8; GLIBC_SIGSET_BYTES];
    to_glibc(a.sa_mask, &mut mask);
    buf[GLIBC_SA_HANDLER..GLIBC_SA_HANDLER + 8].copy_from_slice(&(a.sa_handler as usize).to_ne_bytes());
    // glibc's sigset_t is 128 bytes and sits between handler and flags.
    buf[GLIBC_SA_MASK..GLIBC_SA_MASK + 128].copy_from_slice(&mask[..128]);
    buf[GLIBC_SA_FLAGS..GLIBC_SA_FLAGS + 4].copy_from_slice(&a.sa_flags.to_ne_bytes());
    buf[GLIBC_SA_RESTORER..GLIBC_SA_RESTORER + 8].copy_from_slice(&(a.sa_restorer as usize).to_ne_bytes());
    buf
}

/// The reverse: what glibc filled in, as bionic's struct.
fn sigaction_from_glibc(buf: &[u8; GLIBC_SIGACTION_BYTES]) -> BionicSigaction {
    let word = |at: usize| usize::from_ne_bytes(buf[at..at + 8].try_into().expect("an 8-byte slice"));
    let mut mask = [0u8; GLIBC_SIGSET_BYTES];
    mask[..128].copy_from_slice(&buf[GLIBC_SA_MASK..GLIBC_SA_MASK + 128]);
    BionicSigaction {
        sa_flags: c_int::from_ne_bytes(buf[GLIBC_SA_FLAGS..GLIBC_SA_FLAGS + 4].try_into().expect("a 4-byte slice")),
        _pad: 0,
        sa_handler: word(GLIBC_SA_HANDLER) as *mut c_void,
        sa_mask: from_glibc(&mask),
        sa_restorer: word(GLIBC_SA_RESTORER) as *mut c_void,
    }
}

/// `sigaction(2)` over glibc, in bionic's types. `None` for `act` queries only.
fn sigaction_translated(sig: c_int, act: Option<&BionicSigaction>, want_old: bool) -> (c_int, Option<BionicSigaction>) {
    let new_buf = act.map(sigaction_to_glibc);
    let mut old_buf = [0u8; GLIBC_SIGACTION_BYTES];
    let new_ptr = new_buf.as_ref().map_or(std::ptr::null(), |b| b.as_ptr() as *const c_void);
    let old_ptr = if want_old { old_buf.as_mut_ptr() as *mut c_void } else { std::ptr::null_mut() };
    // SAFETY: both buffers exceed glibc's struct sigaction.
    let rc = unsafe { sigaction(sig, new_ptr, old_ptr) };
    (rc, (want_old && rc == 0).then(|| sigaction_from_glibc(&old_buf)))
}

unsafe extern "C" fn b_sigaction(
    sig: c_int,
    act: *const BionicSigaction,
    old: *mut BionicSigaction,
) -> c_int {
    // SAFETY: bionic's contract is null or a pointer to its 32-byte struct
    // sigaction, valid for the call.
    let act = unsafe { act.as_ref() };
    let (rc, previous) = sigaction_translated(sig, act, !old.is_null());
    if let Some(previous) = previous {
        // SAFETY: `old` is non-null (that is what `want_old` was) and is
        // bionic's 32-byte struct, which is where the previous action goes.
        unsafe { old.write(previous) };
    }
    rc
}

pub fn overrides() -> Vec<(&'static str, *mut c_void)> {
    macro_rules! f {
        ($name:literal, $fn:expr) => {
            ($name, $fn as *const () as *mut c_void)
        };
    }
    vec![
        f!("sigemptyset", b_sigemptyset),
        f!("sigfillset", b_sigfillset),
        f!("sigaddset", b_sigaddset),
        f!("sigdelset", b_sigdelset),
        f!("sigismember", b_sigismember),
        f!("sigprocmask", b_sigprocmask),
        f!("pthread_sigmask", b_pthread_sigmask),
        f!("sigaction", b_sigaction),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bionic_sigset_is_eight_bytes() {
        // The whole reason this module exists. If this ever becomes 128, the
        // wrappers are no longer translating anything.
        assert_eq!(std::mem::size_of::<BionicSigset>(), 8);
        assert_eq!(std::mem::size_of::<BionicSigaction>(), 32);
    }

    #[test]
    fn set_operations_stay_inside_eight_bytes() {
        // A guard word immediately after the set catches the 120-byte overrun
        // that passing these straight to glibc would cause.
        #[repr(C)]
        struct Guarded {
            set: BionicSigset,
            guard: u64,
        }
        let mut g = Guarded { set: 0, guard: 0xDEAD_BEEF_DEAD_BEEF };

        assert_eq!(b_sigemptyset(&mut g.set), 0);
        assert_eq!(g.guard, 0xDEAD_BEEF_DEAD_BEEF, "sigemptyset wrote past the set");

        assert_eq!(b_sigfillset(&mut g.set), 0);
        assert_eq!(g.guard, 0xDEAD_BEEF_DEAD_BEEF, "sigfillset wrote past the set");

        b_sigemptyset(&mut g.set);
        assert_eq!(b_sigaddset(&mut g.set, 9 /* SIGKILL */), 0);
        assert_eq!(b_sigismember(&g.set, 9), 1);
        assert_eq!(b_sigismember(&g.set, 10), 0);
        assert_eq!(g.guard, 0xDEAD_BEEF_DEAD_BEEF, "sigaddset wrote past the set");
    }

    #[test]
    fn masks_survive_a_round_trip_through_glibc() {
        let mut buf = [0u8; GLIBC_SIGSET_BYTES];
        // SIGINT(2), SIGUSR1(10), SIGTERM(15).
        let mask: BionicSigset = (1 << 1) | (1 << 9) | (1 << 14);
        to_glibc(mask, &mut buf);
        assert_eq!(from_glibc(&buf), mask);
    }

    #[test]
    fn pthread_sigmask_reports_the_previous_mask() {
        let mut old: BionicSigset = 0;
        // SIG_SETMASK is 2 on Linux; blocking nothing is always safe.
        let empty: BionicSigset = 0;
        assert_eq!(b_pthread_sigmask(2, &empty, &mut old), 0);
    }

    #[test]
    fn a_sigaction_round_trips_through_glibc_layout() {
        let a = BionicSigaction {
            sa_flags: 0x1234_5678,
            _pad: 0,
            sa_handler: 0x1111_2222_3333_4444usize as *mut c_void,
            sa_mask: (1 << 1) | (1 << 9) | (1 << 14),
            sa_restorer: 0x5555_6666_7777_8888usize as *mut c_void,
        };
        let b = sigaction_from_glibc(&sigaction_to_glibc(&a));
        assert_eq!(b.sa_flags, a.sa_flags);
        assert_eq!(b.sa_handler, a.sa_handler);
        assert_eq!(b.sa_mask, a.sa_mask);
        assert_eq!(b.sa_restorer, a.sa_restorer);
    }

    #[test]
    fn the_translated_layout_is_the_one_glibc_reads() {
        // The offsets are hand-written (see the constants), so compare them with
        // the real struct rather than with themselves: handler first, flags
        // after the 128-byte mask, restorer last.
        let a = BionicSigaction {
            sa_flags: libc::SA_RESTART,
            _pad: 0,
            sa_handler: 0x1000usize as *mut c_void,
            sa_mask: 1 << 9,
            sa_restorer: 0x2000usize as *mut c_void,
        };
        let buf = sigaction_to_glibc(&a);
        // SAFETY: `libc::sigaction` is plain data, and the buffer is larger.
        let real: libc::sigaction = unsafe { std::ptr::read_unaligned(buf.as_ptr() as *const libc::sigaction) };
        assert_eq!(real.sa_sigaction, 0x1000);
        assert_eq!(real.sa_flags, libc::SA_RESTART);
        assert_eq!(real.sa_restorer.map(|f| f as usize), Some(0x2000));
        // SAFETY: `real.sa_mask` is a glibc sigset this test just built.
        assert_eq!(unsafe { libc::sigismember(&real.sa_mask, 10) }, 1);
        assert_eq!(unsafe { libc::sigismember(&real.sa_mask, 2) }, 0);
    }

    #[test]
    fn b_sigaction_reports_the_previous_action_and_installs_the_new_one() {
        unsafe extern "C" fn handler(_: c_int) {}
        const SIG: c_int = 12; // SIGUSR2, which nothing else here installs.
        let mut original: libc::sigaction = unsafe { std::mem::zeroed() };
        // SAFETY: querying the current action only.
        unsafe { libc::sigaction(SIG, std::ptr::null(), &mut original) };

        let new = BionicSigaction {
            sa_flags: libc::SA_RESTART,
            _pad: 0,
            sa_handler: handler as *const () as *mut c_void,
            sa_mask: 0,
            sa_restorer: std::ptr::null_mut(),
        };
        let mut old: BionicSigaction = BionicSigaction {
            sa_flags: -1,
            _pad: 0,
            sa_handler: 1 as *mut c_void,
            sa_mask: u64::MAX,
            sa_restorer: 1 as *mut c_void,
        };
        assert_eq!(b_sigaction(SIG, &new, &mut old), 0);
        assert_eq!(old.sa_handler as usize, original.sa_sigaction, "the previous handler must come back");

        // And what was installed reads back as what was asked for.
        let mut now = BionicSigaction { sa_flags: 0, _pad: 0, sa_handler: std::ptr::null_mut(), sa_mask: 0, sa_restorer: std::ptr::null_mut() };
        assert_eq!(b_sigaction(SIG, std::ptr::null(), &mut now), 0);
        assert_eq!(now.sa_handler, new.sa_handler);
        assert_ne!(now.sa_flags & libc::SA_RESTART, 0);

        // Put it back, so no other test sees a handler of ours.
        // SAFETY: restoring the action read above.
        unsafe { libc::sigaction(SIG, &original, std::ptr::null_mut()) };
    }
}
