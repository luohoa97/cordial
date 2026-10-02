//! M7: pthread keys as bionic keeps them, `pthread_getspecific` and
//! `pthread_setspecific` run as guest code in the stub page (`keys.rs`).
//! Each case is checked against what bionic's `pthread_key.cpp` returns for
//! it, entered the way the engine enters them: through the translator.
#![cfg(all(feature = "dynarmic", target_arch = "x86_64"))]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use cordial_guest::{guest_call, key_clean_all, Options, Runtime, KEYS};

fn get(rt: &Arc<Runtime>, f: u64, key: u64) -> u64 {
    guest_call(rt, f, &[key], &[]).expect("getspecific returned").x0
}

fn set(rt: &Arc<Runtime>, f: u64, key: u64, v: u64) -> u32 {
    guest_call(rt, f, &[key, v], &[]).expect("setspecific returned").x0 as u32
}

#[test]
fn m7_keys_match_bionic() {
    let rt = Runtime::new(Options::default());
    let (g, s) = rt.key_functions();
    let k = rt.key_create(0).unwrap() as u64;
    assert_eq!(k as u32 & 0x8000_0000, 0x8000_0000, "bionic's keys carry KEY_VALID_FLAG");
    assert_eq!(get(&rt, g, k), 0, "a new key reads null");
    assert_eq!(set(&rt, s, k, 0x1234), 0);
    assert_eq!(get(&rt, g, k), 0x1234);
    assert_eq!(rt.key_value(k as u32), Some(0x1234), "the host reads what the guest wrote");
    // The upper half of x0 is unspecified for an int argument.
    assert_eq!(get(&rt, g, k | 0xdead_beef_0000_0000), 0x1234);

    // Another thread's value is its own.
    let rt2 = rt.clone();
    std::thread::spawn(move || {
        assert_eq!(get(&rt2, g, k), 0);
        assert_eq!(set(&rt2, s, k, 7), 0);
        assert_eq!(get(&rt2, g, k), 7);
    }).join().unwrap();
    assert_eq!(get(&rt, g, k), 0x1234);

    // Delete, then a new key in the same slot: the old value must not
    // show through, which is what bionic's sequence numbers are for.
    assert_eq!(rt.key_delete(k as u32), 0);
    assert_eq!(get(&rt, g, k), 0, "a deleted key reads null");
    assert_eq!(set(&rt, s, k, 5), 22, "setspecific on a deleted key is EINVAL");
    assert_eq!(rt.key_delete(k as u32), 22, "deleting twice is EINVAL");
    let k2 = rt.key_create(0).unwrap() as u64;
    assert_eq!(k2, k, "the slot is reused");
    assert_eq!(get(&rt, g, k2), 0, "the reused slot does not show the old value");

    // Invalid keys: bionic's KeyInValidRange.
    for bad in [0u64, 5, 0x8000_0000 + KEYS as u64, 0xffff_ffff] {
        assert_eq!(get(&rt, g, bad), 0, "getspecific({bad:#x})");
        assert_eq!(set(&rt, s, bad, 1), 22, "setspecific({bad:#x})");
    }

    // PTHREAD_KEYS_MAX, then EAGAIN.
    let mut made = 1;
    while rt.key_create(0).is_ok() {
        made += 1;
    }
    assert_eq!(made, KEYS);
    assert_eq!(rt.key_create(0), Err(11));
}

static DTOR_SEEN: AtomicU64 = AtomicU64::new(0);

/// Destructors: only for a non-null value, value cleared first, and on the
/// exiting thread. The destructor is guest code that stores its argument
/// into a host word, `ldr x9, <literal>; str x0, [x9]; ret`.
#[test]
fn m7_key_destructors_run_at_thread_exit() {
    let rt = Runtime::new(Options::default());
    let (g, s) = rt.key_functions();
    let addr = &DTOR_SEEN as *const AtomicU64 as u64;
    // ldr x9, #16; str x0, [x9]; ret; nop; .quad addr
    let code: Vec<u32> = vec![0x5800_0089, 0xf900_0120, 0xd65f_03c0, 0xd503_201f,
                              addr as u32, (addr >> 32) as u32];
    let bytes: Vec<u8> = code.iter().flat_map(|w| w.to_le_bytes()).collect();
    let dtor = cordial_guest::Mapping::with_contents(&bytes, 0);
    let k = rt.key_create(dtor.addr()).unwrap() as u64;
    let k_null = rt.key_create(dtor.addr()).unwrap() as u64;
    let rt2 = rt.clone();
    std::thread::spawn(move || {
        assert_eq!(set(&rt2, s, k, 0xabc), 0);
        assert_eq!(set(&rt2, s, k_null, 0), 0);
        key_clean_all(&rt2).unwrap();
        assert_eq!(get(&rt2, g, k), 0, "cleared before the destructor ran");
    }).join().unwrap();
    assert_eq!(DTOR_SEEN.load(Ordering::SeqCst), 0xabc);
}

/// The hints complete: `sevl; wfe; yield; sev; add x0, x0, #1; ret` returns
/// its argument plus one. Control: WFI, which is not a hint a thread can
/// wait out, still stops, at the instruction.
#[test]
fn m7_hints_complete_and_wfi_still_stops() {
    let rt = Runtime::new(Options::default());
    let code = |w: &[u32]| {
        let bytes: Vec<u8> = w.iter().flat_map(|w| w.to_le_bytes()).collect();
        cordial_guest::Mapping::with_contents(&bytes, 0)
    };
    let hints = code(&[0xd503_20bf, 0xd503_205f, 0xd503_203f, 0xd503_209f, 0x9100_0400, 0xd65f_03c0]);
    assert_eq!(guest_call(&rt, hints.addr(), &[41], &[]).expect("hints returned").x0, 42);
    let wfi = code(&[0xd503_207f, 0xd65f_03c0]);
    assert_eq!(guest_call(&rt, wfi.addr(), &[], &[]).unwrap_err(),
               cordial_guest::Fault::Exception { pc: wfi.addr(), kind: cordial_guest::Exception::WaitForInterrupt });
}

extern "C" {
    fn memcpy(d: *mut u8, s: *const u8, n: usize) -> *mut u8;
    fn memmove(d: *mut u8, s: *const u8, n: usize) -> *mut u8;
    fn memset(d: *mut u8, c: i32, n: usize) -> *mut u8;
    fn memcmp(a: *const u8, b: *const u8, n: usize) -> i32;
}

/// `memcpy`, `memmove`, `memset` and `memcmp` as guest code (`string.rs`),
/// at every size from 0 to 160 and every alignment mod 16, against glibc's,
/// through the translator. Up to 128 bytes no SVC is taken; past it each
/// call reaches the host function's stub exactly once.
#[test]
#[allow(unsafe_code)]
fn m7_string_functions_match_libc() {
    use cordial_guest::{Ret, Ty};
    let rt = Runtime::new(Options::default());
    let host = |name, f: *const std::ffi::c_void, args: &'static [Ty], ret| {
        let stub = rt.register_host(name, f, args, ret);
        rt.string_function(name, stub).unwrap()
    };
    let cpy = host("memcpy", memcpy as *const _, &[Ty::Ptr, Ty::Ptr, Ty::U64], Ret::Int(Ty::Ptr));
    let mov = host("memmove", memmove as *const _, &[Ty::Ptr, Ty::Ptr, Ty::U64], Ret::Int(Ty::Ptr));
    let set = host("memset", memset as *const _, &[Ty::Ptr, Ty::I32, Ty::U64], Ret::Int(Ty::Ptr));
    let cmp = host("memcmp", memcmp as *const _, &[Ty::Ptr, Ty::Ptr, Ty::U64], Ret::Int(Ty::I32));
    let call = |f, args: &[u64]| {
        let before = rt.svc_calls();
        let r = guest_call(&rt, f, args, &[]).expect("returned").x0;
        (r, rt.svc_calls() - before)
    };
    let svcs = |n: usize| u64::from(n > 128);
    let pattern = |i: usize| (i as u8).wrapping_mul(37).wrapping_add(11);

    for n in 0..=160usize {
        for off in 0..16usize {
            // memcpy, between distinct buffers with guard bytes either side.
            let src: Vec<u8> = (0..n + 64).map(pattern).collect();
            let mut got = vec![0xeeu8; n + 64];
            let mut want = got.clone();
            let d = got.as_mut_ptr() as u64 + 32 + off as u64 % 8;
            let s = src.as_ptr() as u64 + off as u64;
            assert_eq!(call(cpy, &[d, s, n as u64]), (d, svcs(n)), "memcpy n={n} off={off}");
            // SAFETY: both ranges are inside their buffers.
            unsafe { memcpy(want.as_mut_ptr().add(32 + off % 8), src.as_ptr().add(off), n) };
            assert_eq!(got, want, "memcpy n={n} off={off}");

            // memmove, overlapping in both directions by `off`.
            for fwd in [false, true] {
                let mut got: Vec<u8> = (0..n + 64).map(pattern).collect();
                let mut want = got.clone();
                let (da, sa) = if fwd { (16 + off, 16) } else { (16, 16 + off) };
                let base = got.as_mut_ptr() as u64;
                assert_eq!(call(mov, &[base + da as u64, base + sa as u64, n as u64]).1, svcs(n));
                // SAFETY: both ranges are inside the buffer.
                unsafe { memmove(want.as_mut_ptr().add(da), want.as_ptr().add(sa), n) };
                assert_eq!(got, want, "memmove n={n} off={off} fwd={fwd}");
            }

            // memset, with the byte in the upper bits of w1 ignored.
            let mut got = vec![0x11u8; n + 64];
            let mut want = got.clone();
            let d = got.as_mut_ptr() as u64 + 16 + off as u64;
            assert_eq!(call(set, &[d, 0xdead_be00 | 0xa5, n as u64]), (d, svcs(n)), "memset n={n}");
            // SAFETY: inside the buffer.
            unsafe { memset(want.as_mut_ptr().add(16 + off), 0xa5, n) };
            assert_eq!(got, want, "memset n={n} off={off}");
        }

        // memcmp: equal, then one differing byte at each position, each way,
        // with a byte whose order differs signed and unsigned.
        let a: Vec<u8> = (0..n + 16).map(pattern).collect();
        for off in [0usize, 3, 8] {
            let mut b = a.clone();
            let pa = a.as_ptr() as u64 + off as u64;
            let (r, k) = call(cmp, &[pa, b.as_ptr() as u64 + off as u64, n as u64]);
            assert_eq!((r as i32, k), (0, svcs(n)), "memcmp equal n={n}");
            for at in 0..n {
                for (x, y) in [(0x01u8, 0xf0u8), (0xf0, 0x01)] {
                    let mut a2 = a.clone();
                    a2[off + at] = x;
                    b[off + at] = y;
                    let (r, _) = call(cmp, &[a2.as_ptr() as u64 + off as u64, b.as_ptr() as u64 + off as u64, n as u64]);
                    // SAFETY: both ranges are inside their buffers.
                    let w = unsafe { memcmp(a2.as_ptr().add(off), b.as_ptr().add(off), n) };
                    assert_eq!((r as i32).signum(), w.signum(), "memcmp n={n} at={at} {x:#x} vs {y:#x}");
                    b[off + at] = a[off + at];
                }
            }
        }
    }
}

/// `clock_gettime(CLOCK_MONOTONIC)` as guest code (`clock.rs`) agrees with
/// the host's to within the 2 ns the 600 MHz counter loses, never ahead of
/// it, without an SVC; any other clock reaches the host's stub.
#[test]
#[allow(unsafe_code)]
fn m7_clock_gettime_monotonic_from_the_counter() {
    use cordial_guest::{Ret, Ty};
    extern "C" {
        fn clock_gettime(clk: i32, ts: *mut [i64; 2]) -> i32;
    }
    let rt = Runtime::new(Options::default());
    let stub = rt.register_host("clock_gettime", clock_gettime as *const _, &[Ty::I32, Ty::Ptr], Ret::Int(Ty::I32));
    let f = rt.clock_function(stub);
    let ns = |t: [i64; 2]| t[0] as i128 * 1_000_000_000 + t[1] as i128;
    let host = || {
        let mut t = [0i64; 2];
        // SAFETY: a local timespec.
        unsafe { clock_gettime(1, &mut t) };
        t
    };
    let mut last = 0i128;
    for _ in 0..10_000 {
        let mut t = [0i64; 2];
        let before = host();
        let svcs = rt.svc_calls();
        let r = guest_call(&rt, f, &[1, &mut t as *mut _ as u64], &[]).expect("returned").x0;
        assert_eq!(rt.svc_calls(), svcs, "CLOCK_MONOTONIC took an SVC");
        let after = host();
        assert_eq!(r as i32, 0);
        assert!((0..1_000_000_000).contains(&t[1]), "tv_nsec {}", t[1]);
        assert!(ns(t) >= ns(before) - 2 && ns(t) <= ns(after), "{t:?} not within {before:?}..{after:?}");
        assert!(ns(t) >= last, "went backwards");
        last = ns(t);
    }
    // CLOCK_REALTIME, by the host.
    let mut t = [0i64; 2];
    let svcs = rt.svc_calls();
    let r = guest_call(&rt, f, &[0, &mut t as *mut _ as u64], &[]).expect("returned").x0;
    assert_eq!((r as i32, rt.svc_calls() - svcs), (0, 1));
    assert!(t[0] > 1_700_000_000, "CLOCK_REALTIME is the epoch clock: {t:?}");
}
