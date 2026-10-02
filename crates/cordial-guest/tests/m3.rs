//! M3 of docs/vr/dynarmic-design.md §8, the parts of it that are mechanism
//! rather than engine: a host entry point calling a guest function the way
//! libjnivm calls a registered native, and the translator's SIGSEGV handler
//! coexisting with the one Rust's runtime installs first.
#![cfg(target_arch = "x86_64")]
#![allow(unsafe_code)]

use std::process::Command;
use std::sync::Arc;

use cordial_guest::{guest_call, host_entry, Mapping, Options, Ret, Runtime, Ty};

#[allow(dead_code)]
mod guest {
    include!(concat!(env!("OUT_DIR"), "/guest_syms.rs"));
}
static IMAGE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/guest.bin"));

type Many = extern "C" fn(i64, i64, i8, u16, i16, i32, i64, f32, f64, i32, i64, f32, f64, f64, f64, f64,
                          f64, f64, f64, i32, i64) -> f64;

/// The same computation as `guest_many` in tests/guest/m1.c, natively.
#[allow(clippy::too_many_arguments)]
extern "C" fn native_many(env: i64, obj: i64, b: i8, c: u16, s: i16, i: i32, j: i64, f: f32, d: f64, i2: i32,
                          j2: i64, f2: f32, d2: f64, d3: f64, d4: f64, d5: f64, d6: f64, d7: f64, d8: f64,
                          i3: i32, j3: i64) -> f64 {
    // As C converts each argument to unsigned long: sign-extending the
    // signed ones, zero-extending the unsigned, and floats by their bits.
    let mut h = env as u64;
    for v in [obj as u64, b as i64 as u64, c as u64, s as i64 as u64, i as i64 as u64, j as u64,
              f.to_bits() as u64, d.to_bits(), i2 as i64 as u64, j2 as u64, f2.to_bits() as u64, d2.to_bits(),
              d3.to_bits(), d4.to_bits(), d5.to_bits(), d6.to_bits(), d7.to_bits(), d8.to_bits(),
              i3 as i64 as u64, j3 as u64] {
        h = h.wrapping_mul(31).wrapping_add(v);
    }
    f64::from_bits(h)
}

/// A host entry built from the signature, called as a plain host function
/// pointer, gives the same bits as the native function for 1000 argument
/// sets; and its argument map is applied (the JNIEnv* swap).
#[test]
#[cfg_attr(cordial_guest_no_image, ignore = "no arm64 test image here; cordial-guest's build warning says why")]
fn m3_host_entry_matches_native_with_stack_overflow_both_ways() {
    use Ty::*;
    let rt: Arc<Runtime> = Runtime::new(Options::default());
    let img = Mapping::with_contents(IMAGE, 0x1000);
    let args = vec![I64, I64, I8, U16, I16, I32, I64, F32, F64, I32, I64, F32, F64, F64, F64, F64, F64, F64,
                    F64, I32, I64];
    let map: cordial_guest::ArgMap = Box::new(|i, v| if i == 0 { v.wrapping_add(1000) } else { v });
    let p = host_entry(&rt, "guest_many", img.addr() + guest::GUEST_MANY, args, Ret::F64, Some(map)).unwrap();
    // SAFETY: the entry was built for exactly this signature.
    let f: Many = unsafe { std::mem::transmute(p) };
    let mut x = 0x2545_f491_4f6c_dd1du64;
    let mut next = || {
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    };
    for case in 0..1000 {
        let r: Vec<u64> = (0..21).map(|_| next()).collect();
        let small = |k: usize| (r[k] % 2001) as i64 - 1000;
        let fl = |k: usize| small(k) as f64 / 7.0;
        let a = (small(0), small(1), small(2) as i8, r[3] as u16, small(4) as i16, small(5) as i32, small(6),
                 fl(7) as f32, fl(8), small(9) as i32, small(10), fl(11) as f32, fl(12), fl(13), fl(14), fl(15),
                 fl(16), fl(17), fl(18), small(19) as i32, small(20));
        let got = f(a.0, a.1, a.2, a.3, a.4, a.5, a.6, a.7, a.8, a.9, a.10, a.11, a.12, a.13, a.14, a.15, a.16,
                    a.17, a.18, a.19, a.20);
        let want = native_many(a.0 + 1000, a.1, a.2, a.3, a.4, a.5, a.6, a.7, a.8, a.9, a.10, a.11, a.12, a.13,
                               a.14, a.15, a.16, a.17, a.18, a.19, a.20);
        assert_eq!(got.to_bits(), want.to_bits(), "case {case}: {r:x?}");
    }
}

/// Runs this test binary again with `scenario` in the environment and
/// returns its exit status and stderr. Each scenario ends the process, so
/// it has to be a separate one.
fn child(scenario: &str) -> (std::process::ExitStatus, String) {
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "m3_signal_child", "--nocapture", "--test-threads=1"])
        .env("CORDIAL_M3_SCENARIO", scenario)
        .output()
        .unwrap();
    (out.status, String::from_utf8_lossy(&out.stderr).into_owned())
}

/// Not a test on its own: the body of each signal scenario, run in a child.
#[test]
#[cfg_attr(cordial_guest_no_image, ignore = "no arm64 test image here; cordial-guest's build warning says why")]
fn m3_signal_child() {
    let Ok(scenario) = std::env::var("CORDIAL_M3_SCENARIO") else { return };
    // Creating a Jit installs dynarmic's process-wide SIGSEGV handler, after
    // the one Rust's runtime installed before main; it chains to that one.
    let rt = Runtime::new(Options::default());
    let img = Mapping::with_contents(IMAGE, 0x1000);
    let r = guest_call(&rt, img.addr() + guest::GUEST_ADD, &[2, 3], &[]).unwrap();
    assert_eq!(r.x0, 5);
    eprintln!("jit up");
    match scenario.as_str() {
        // A host null dereference, outside any translated code.
        // SAFETY: none; faulting is the point, in a child process.
        "host" => unsafe {
            std::ptr::read_volatile(std::ptr::null::<u64>().wrapping_add(1));
        },
        // Guest code loading through a wild pointer: the load is translated
        // code reaching memory through fastmem, so the fault is taken in
        // the Jit's own code first.
        "guest" => {
            let _ = guest_call(&rt, img.addr() + guest::GUEST_LOAD, &[0x10], &[]);
        }
        // Stack overflow on the thread whose alternate signal stack dynarmic
        // replaced: Rust's own handler must still recognise the guard page.
        "overflow" => {
            #[allow(unconditional_recursion)]
            fn deep(n: u64) -> u64 {
                let a = [n; 64];
                std::hint::black_box(&a);
                deep(n + 1) + a[0]
            }
            std::hint::black_box(deep(0));
        }
        _ => {}
    }
    eprintln!("scenario {scenario} did not end the process");
    std::process::exit(99);
}

#[test]
#[cfg_attr(cordial_guest_no_image, ignore = "no arm64 test image here; cordial-guest's build warning says why")]
fn m3_host_fault_after_jit_still_dies_by_sigsegv() {
    use std::os::unix::process::ExitStatusExt;
    let (st, err) = child("host");
    assert!(err.contains("jit up"), "{err}");
    assert_eq!(st.signal(), Some(11), "{st:?}\n{err}");
}

#[test]
#[cfg_attr(cordial_guest_no_image, ignore = "no arm64 test image here; cordial-guest's build warning says why")]
fn m3_guest_wild_pointer_dies_by_sigsegv() {
    use std::os::unix::process::ExitStatusExt;
    let (st, err) = child("guest");
    assert!(err.contains("jit up"), "{err}");
    assert_eq!(st.signal(), Some(11), "{st:?}\n{err}");
}

#[test]
#[cfg_attr(cordial_guest_no_image, ignore = "no arm64 test image here; cordial-guest's build warning says why")]
fn m3_stack_overflow_after_jit_is_still_named_by_rust() {
    use std::os::unix::process::ExitStatusExt;
    let (st, err) = child("overflow");
    assert!(err.contains("jit up"), "{err}");
    assert!(err.contains("has overflowed its stack"), "{err}");
    assert_eq!(st.signal(), Some(6), "{st:?}\n{err}");
}
