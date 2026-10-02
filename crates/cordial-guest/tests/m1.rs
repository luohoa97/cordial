//! M1 of docs/vr/dynarmic-design.md §8: stubs, the generic call builder,
//! nested Jits, and guest threads, checked against the same host functions
//! called natively.
//!
//! The guest side is `tests/guest/m1.c`, compiled for aarch64 by build.rs, so
//! every call into a stub is a call sequence Clang emitted for AAPCS64.
#![cfg(all(feature = "dynarmic", target_arch = "x86_64"))]
// The native side of every comparison is a raw libc call, which is the
// point: the reference is the host function itself, not a Rust stand-in.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void, CStr};
use std::sync::Arc;
use std::time::Instant;

use cordial_guest::{guest_call, thread_jit_count, thunks, Mapping, MonitorMode, Options, Ret, Runtime, Ty};

#[allow(dead_code)]
mod guest {
    include!(concat!(env!("OUT_DIR"), "/guest_syms.rs"));
}
static IMAGE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/guest.bin"));

extern "C" {
    fn strlen(s: *const c_char) -> usize;
    fn snprintf(buf: *mut c_char, n: usize, fmt: *const c_char, ...) -> c_int;
    fn qsort(base: *mut c_void, n: usize, size: usize,
             cmp: extern "C" fn(*const c_void, *const c_void) -> c_int);
    fn pthread_join(t: u64, ret: *mut *mut c_void) -> c_int;
}

/// xorshift64*: deterministic, so a failing case can be replayed by seed.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    /// A double that is sometimes ordinary, sometimes a raw bit pattern
    /// (NaNs, infinities, subnormals), so the bits are what is compared.
    fn double(&mut self) -> f64 {
        match self.below(4) {
            0 => f64::from_bits(self.next()),
            1 => (self.next() as i64 as f64) / 1e6,
            2 => [0.0, -0.0, f64::INFINITY, f64::NEG_INFINITY, f64::NAN, f64::MIN_POSITIVE / 3.0]
                [self.below(6) as usize],
            _ => (self.below(2000) as f64 - 1000.0) / 7.0,
        }
    }
    fn cstring(&mut self, max: u64) -> Vec<u8> {
        let n = self.below(max + 1);
        let mut v: Vec<u8> = (0..n).map(|_| 1 + self.below(255) as u8).collect();
        v.push(0);
        v
    }
}

struct World {
    rt: Arc<Runtime>,
    img: Mapping,
    strlen: u64,
    snprintf: u64,
    qsort: u64,
    create: u64,
    join: u64,
}

fn world(monitor: MonitorMode) -> World {
    let rt = Runtime::new(Options { monitor, ..Options::default() });
    let img = Mapping::with_contents(IMAGE, 0x1000);
    World {
        strlen: rt.register_host("strlen", strlen as *const c_void, &[Ty::Ptr], Ret::Int(Ty::U64)),
        snprintf: rt.register("snprintf",
                              thunks::printf_like("snprintf", snprintf as *const c_void,
                                                  &[Ty::Ptr, Ty::U64, Ty::Ptr], 2)),
        qsort: rt.register("qsort", thunks::qsort()),
        create: rt.register("pthread_create", thunks::pthread_create_thunk()),
        join: rt.register_host("pthread_join", pthread_join as *const c_void,
                               &[Ty::U64, Ty::Ptr], Ret::Int(Ty::I32)),
        rt,
        img,
    }
}

impl World {
    fn at(&self, off: u64) -> u64 {
        self.img.addr() + off
    }
}

#[test]
fn m1_strlen_matches_native() {
    let w = world(MonitorMode::Global);
    let mut rng = Rng(0x5eed_0001);
    for case in 0..1000 {
        let s = rng.cstring(300);
        // SAFETY: NUL-terminated.
        let native = unsafe { strlen(s.as_ptr().cast()) } as u64;
        let r = guest_call(&w.rt, w.at(guest::GUEST_STRLEN), &[w.strlen, s.as_ptr() as u64], &[])
            .unwrap_or_else(|f| panic!("case {case}: {f:?}"));
        assert_eq!(r.x0, native, "case {case}");
    }
    println!("M1 strlen: 1000/1000 identical to native");
}

#[repr(C)]
struct FmtArgs {
    a: i64,
    b: f64,
    c: i32,
    d: f64,
    s: *const c_char,
    e: u32,
    g: f64,
    h: i64,
    i: f64,
    j: i32,
    k: f64,
    l: f64,
    m: f64,
    o: f64,
    p: f64,
    q: i32,
    r: i8,
    t: i16,
}

#[test]
fn m1_snprintf_matches_native() {
    let w = world(MonitorMode::Global);
    let fmt_ptr = guest_call(&w.rt, w.at(guest::GUEST_FMT), &[], &[]).unwrap().x0 as *const c_char;
    // SAFETY: the guest returned the address of its own NUL-terminated FMT.
    println!("format: {:?}", unsafe { CStr::from_ptr(fmt_ptr) });
    let mut rng = Rng(0x5eed_0002);
    let mut example = None;
    let (mut native_time, mut guest_time) = (std::time::Duration::ZERO, std::time::Duration::ZERO);
    for case in 0..1000 {
        let s = rng.cstring(20);
        let x = FmtArgs {
            a: rng.next() as i64, b: rng.double(), c: rng.next() as i32, d: rng.double(),
            s: s.as_ptr().cast(), e: rng.next() as u32, g: rng.double(), h: rng.next() as i64,
            i: rng.double(), j: rng.next() as i32, k: rng.double(), l: rng.double(),
            m: rng.double(), o: rng.double(), p: rng.double(),
            q: (rng.below(40) as i32) - 20, r: (32 + rng.below(95)) as i8, t: rng.next() as i16,
        };
        let n = rng.below(700) as usize;
        let mut native = vec![0xa5u8; 800];
        let mut guest = vec![0xa5u8; 800];
        let t0 = Instant::now();
        // SAFETY: the same format and arguments the guest passes, in the
        // same order (tests/guest/m1.c), with C's default promotions spelled.
        let nr = unsafe {
            snprintf(native.as_mut_ptr().cast(), n, fmt_ptr, x.a, x.b, x.c, x.d, x.s, x.e, x.g,
                     x.h, x.i, x.j, x.k, x.l, x.m, x.o, x.p, x.q, x.r as c_int, x.t as c_int,
                     x.b, x.s, x.e, x.c, x.q)
        };
        let t1 = Instant::now();
        native_time += t1 - t0;
        let r = guest_call(&w.rt, w.at(guest::GUEST_SNPRINTF),
                           &[w.snprintf, guest.as_mut_ptr() as u64, n as u64, &x as *const _ as u64], &[])
            .unwrap_or_else(|f| panic!("case {case}: {f:?}"));
        guest_time += t1.elapsed();
        assert_eq!(r.x0 as i32, nr, "case {case}: return value");
        assert_eq!(guest, native, "case {case}: buffer (n = {n})");
        if example.is_none() && n > 300 {
            example = Some(String::from_utf8_lossy(&guest[..guest.iter().position(|&b| b == 0).unwrap()]).into_owned());
        }
    }
    println!("M1 snprintf: 1000/1000 identical to native (return value and all 800 buffer bytes)");
    println!("  e.g. {}", example.unwrap_or_default());
    println!("  time in snprintf: native {:?}, through the guest {:?}", native_time, guest_time);
}

extern "C" fn native_cmp_int(a: *const c_void, b: *const c_void) -> c_int {
    // SAFETY: qsort passes pointers to elements of an int array.
    let (x, y) = unsafe { (*(a as *const i32), *(b as *const i32)) };
    (x > y) as c_int - (x < y) as c_int
}

#[test]
fn m1_qsort_with_guest_comparator_matches_native() {
    let w = world(MonitorMode::Global);
    let mut rng = Rng(0x5eed_0003);
    for case in 0..1000 {
        let len = rng.below(200) as usize;
        let mut native: Vec<i32> = (0..len).map(|_| rng.below(50) as i32 - 25 + (rng.next() as i32 >> 20)).collect();
        let mut guest = native.clone();
        // SAFETY: an i32 array with a matching comparator.
        unsafe { qsort(native.as_mut_ptr().cast(), len, 4, native_cmp_int) };
        guest_call(&w.rt, w.at(guest::GUEST_QSORT), &[w.qsort, guest.as_mut_ptr() as u64, len as u64], &[])
            .unwrap_or_else(|f| panic!("case {case}: {f:?}"));
        assert_eq!(guest, native, "case {case}");
    }
    println!("M1 qsort: 1000/1000 identical to native; Jits on this thread: {}", thread_jit_count());
    assert_eq!(thread_jit_count(), 2, "one Jit for the caller and one for the nested comparator");
}

extern "C" fn native_cmp_len(a: *const c_void, b: *const c_void) -> c_int {
    // SAFETY: qsort passes pointers to elements of a char* array.
    let (x, y) = unsafe { (strlen(*(a as *const *const c_char)), strlen(*(b as *const *const c_char))) };
    (x > y) as c_int - (x < y) as c_int
}

/// guest -> host qsort -> guest comparator -> host strlen, twice per compare.
#[test]
fn m1_qsort_nested_three_levels_matches_native() {
    let w = world(MonitorMode::Global);
    let mut rng = Rng(0x5eed_0004);
    let ctx = [w.strlen];
    for case in 0..1000 {
        let strings: Vec<Vec<u8>> = (0..rng.below(60)).map(|_| rng.cstring(12)).collect();
        let mut native: Vec<*const c_char> = strings.iter().map(|s| s.as_ptr().cast()).collect();
        let mut guest = native.clone();
        // SAFETY: an array of C strings with a matching comparator.
        unsafe { qsort(native.as_mut_ptr().cast(), native.len(), 8, native_cmp_len) };
        guest_call(&w.rt, w.at(guest::GUEST_QSORT_NESTED),
                   &[w.qsort, ctx.as_ptr() as u64, guest.as_mut_ptr() as u64, guest.len() as u64], &[])
            .unwrap_or_else(|f| panic!("case {case}: {f:?}"));
        assert_eq!(guest, native, "case {case}");
    }
    println!("M1 nested qsort/strlen: 1000/1000 identical to native; Jits on this thread: {}",
             thread_jit_count());
}

/// Design §3.1's fixed mapping, spelled as a type list for the same builder:
/// x0..x7 as integers (x6, x7 onto the SysV stack), v0..v7, then the one
/// guest stack word this call has, appended after x6 and x7. Given the right
/// stack-word count, which is the most a descriptor could add to it.
const FIXED: [Ty; 17] = {
    let mut t = [Ty::I64; 17];
    let mut i = 8;
    while i < 16 {
        t[i] = Ty::F64;
        i += 1;
    }
    t
};

/// The case where the two ABIs' stack orders part: an overflowed double
/// before an overflowed int. The classifier matches native on 1000 cases;
/// the design's fixed mapping, as the control, does not.
#[test]
fn m1_snprintf_fp_overflow_before_int_overflow() {
    let w = world(MonitorMode::Global);
    let fixed = w.rt.register("snprintf/fixed-mapping", Box::new(|c| {
        c.host(snprintf as *const c_void, &FIXED, Ret::Int(Ty::I32))
    }));
    let fmt = guest_call(&w.rt, w.at(guest::GUEST_FMT9), &[], &[]).unwrap().x0 as *const c_char;
    let mut rng = Rng(0x5eed_0005);
    let mut fixed_wrong = 0;
    let mut shown = false;
    for case in 0..1000 {
        let d: Vec<f64> = (0..9).map(|_| rng.double()).collect();
        let i: Vec<i32> = (0..4).map(|_| rng.next() as i32).collect();
        let mut native = vec![0u8; 400];
        // SAFETY: FMT9's nine doubles and four ints, in its order.
        let nr = unsafe {
            snprintf(native.as_mut_ptr().cast(), 400, fmt, d[0], d[1], d[2], d[3], d[4], d[5],
                     d[6], d[7], d[8], i[0], i[1], i[2], i[3])
        };
        let run = |stub: u64| {
            let mut buf = vec![0u8; 400];
            let r = guest_call(&w.rt, w.at(guest::GUEST_SNPRINTF9),
                               &[stub, buf.as_mut_ptr() as u64, 400, d.as_ptr() as u64, i.as_ptr() as u64], &[])
                .unwrap();
            (r.x0 as i32, buf)
        };
        let (gr, gbuf) = run(w.snprintf);
        assert_eq!((gr, &gbuf), (nr, &native), "case {case}");
        let (_, fbuf) = run(fixed);
        if fbuf != native {
            fixed_wrong += 1;
            if !shown {
                shown = true;
                let z = |b: &[u8]| String::from_utf8_lossy(&b[..b.iter().position(|&c| c == 0).unwrap()]).into_owned();
                println!("  native: {}\n  fixed:  {}", z(&native), z(&fbuf));
            }
        }
    }
    println!("M1 9 doubles + 4 ints: classifier 1000/1000 identical; design's fixed mapping wrong in {fixed_wrong}/1000");
    assert_eq!(fixed_wrong, 1000);
}

/// dynarmic does not switch MXCSR back to the host's around CallSVC, so a
/// host callee runs under the guest's FPCR as dynarmic translated it.
#[test]
fn m1_host_callee_runs_under_guest_rounding_mode() {
    #[allow(deprecated)]
    extern "C" fn read_mxcsr() -> u64 {
        // SAFETY: reading MXCSR has no preconditions on x86-64.
        unsafe { std::arch::x86_64::_mm_getcsr() as u64 }
    }
    let w = world(MonitorMode::Global);
    let f = w.rt.register_host("read_mxcsr", read_mxcsr as *const c_void, &[], Ret::Int(Ty::U64));
    for (name, fpcr) in [("RN", 0u64), ("RZ", 3 << 22), ("FZ", 1 << 24)] {
        let m = guest_call(&w.rt, w.at(guest::GUEST_WITH_FPCR), &[f, fpcr], &[]).unwrap().x0;
        println!("guest FPCR {name} ({fpcr:#x}) -> host callee sees MXCSR {m:#06x} (host's own {:#06x})",
                 read_mxcsr());
    }
}

#[test]
fn m1_tpidr_is_the_threads_tls_block() {
    let w = world(MonitorMode::Global);
    let a = guest_call(&w.rt, w.at(guest::GUEST_TPIDR), &[], &[]).unwrap().x0;
    let b = std::thread::spawn({
        let rt = w.rt.clone();
        let pc = w.at(guest::GUEST_TPIDR);
        move || guest_call(&rt, pc, &[], &[]).unwrap().x0
    }).join().unwrap();
    println!("TPIDR_EL0: this thread {a:#x}, another {b:#x}");
    assert!(a != 0 && b != 0 && a != b);
}

#[repr(C)]
struct IncArgs {
    counter: *mut i64,
    iterations: i64,
}

fn llsc(mode: MonitorMode) {
    let threads: u64 = std::env::var("CORDIAL_GUEST_LLSC_THREADS").ok().and_then(|v| v.parse().ok()).unwrap_or(64);
    let iters: i64 = std::env::var("CORDIAL_GUEST_LLSC_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(10_000);
    let w = world(mode);
    let mut counter = 0i64;
    let args = IncArgs { counter: &mut counter, iterations: iters };
    let t0 = Instant::now();
    let r = guest_call(&w.rt, w.at(guest::GUEST_SPAWN),
                       &[w.create, w.join, &args as *const _ as u64, threads], &[])
        .unwrap_or_else(|f| panic!("{f:?}"));
    let dt = t0.elapsed();
    // SAFETY: every guest thread has been joined.
    let counter = unsafe { std::ptr::read_volatile(&counter) };
    println!("M1 LL/SC {mode:?}: {threads} threads x {iters} = {} expected, counter {counter}, \
              sum of returns {}, {:.3} s ({:.1} ns per increment)",
             threads as i64 * iters, r.x0 as i64, dt.as_secs_f64(),
             dt.as_nanos() as f64 / (threads as f64 * iters as f64));
    assert_eq!(r.x0 as i64, threads as i64 * iters, "guest_spawn result (negative is a create/join failure)");
    assert_eq!(counter, threads as i64 * iters);
}

#[test]
fn m1_llsc_global_monitor() {
    llsc(MonitorMode::Global);
}

#[test]
fn m1_llsc_global_monitor_inline() {
    llsc(MonitorMode::GlobalInline);
}

#[test]
fn m1_llsc_ignore_global_monitor() {
    llsc(MonitorMode::Ignore);
}

/// The control for the three above: the same loop with a plain load and
/// store would lose increments. Not run as guest code -- the point is only
/// that 64 x N is not a total any racy counter reaches by luck.
#[test]
fn m1_llsc_control_racy_counter_loses_updates() {
    let mut counter = 0i64;
    let p = &mut counter as *mut i64 as usize;
    let hs: Vec<_> = (0..64).map(|_| std::thread::spawn(move || {
        for _ in 0..100_000 {
            // SAFETY: deliberately racy volatile read-modify-write.
            unsafe { let q = p as *mut i64; q.write_volatile(q.read_volatile() + 1) };
        }
    })).collect();
    hs.into_iter().for_each(|h| h.join().unwrap());
    println!("control: racy 64 x 100000 -> {counter} (expected < 6400000)");
    assert!(counter < 6_400_000);
}
