//! Code the guest generates, through the translator: written, made
//! executable, run, then rewritten or unmapped and mapped again, and run
//! again. Every Jit that ran the old code must run the new.
//!
//! The guest side is `tests/guest/code.c`. Its `mmap`, `munmap` and
//! `mprotect` are `cordial_guest::code`'s, the functions the client's thunks
//! call, so this is the client's path and not a stand-in for it.
#![cfg(all(feature = "dynarmic", target_arch = "x86_64"))]
#![allow(unsafe_code)]

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use cordial_guest::{code, guest_call, Call, Fault, Mapping, Options, Runtime};

#[allow(dead_code)]
mod guest {
    include!(concat!(env!("OUT_DIR"), "/guest_syms.rs"));
}
static IMAGE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/guest.bin"));

fn ret(c: &Call, r: Result<Result<u64, i32>, Fault>) -> Result<(), Fault> {
    c.set_x(0, r?.unwrap_or(u64::MAX));
    Ok(())
}

/// One test at a time. The remap tests unmap a page and map it again with
/// MAP_FIXED, and a test running beside them could be given that page in
/// between and then have it replaced under it -- a race between the tests,
/// which a kernel has too, not one in what they test.
static SERIAL: Mutex<()> = Mutex::new(());

struct World {
    _serial: MutexGuard<'static, ()>,
    rt: Arc<Runtime>,
    img: Mapping,
    /// `struct code_api`: mmap, munmap, mprotect.
    api: Box<[u64; 3]>,
    wait: u64,
}

impl World {
    fn new() -> World {
        let serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let rt = Runtime::new(Options::default());
        let img = Mapping::with_contents(IMAGE, 0x1000);
        let api = Box::new([
            rt.register("mmap", Box::new(|c| ret(c, code::mmap(c.x(0), c.x(1), c.x(2), c.x(3), c.x(4), c.x(5), c.x(30))))),
            rt.register("munmap", Box::new(|c| ret(c, code::munmap(c.x(0), c.x(1), c.x(30))))),
            rt.register("mprotect", Box::new(|c| ret(c, code::mprotect(c.x(0), c.x(1), c.x(2), c.x(30))))),
        ]);
        // Blocks in the host until flags[1] is set.
        let wait = rt.register("wait", Box::new(|c| {
            // SAFETY: the guest's flags array, two words.
            let flag = unsafe { &*((c.x(0) + 8) as *const AtomicI64) };
            while flag.load(Ordering::Acquire) == 0 {
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(())
        }));
        World { _serial: serial, rt, img, api, wait }
    }

    fn at(&self, off: u64) -> u64 {
        self.img.addr() + off
    }

    fn call(&self, off: u64, args: &[u64]) -> u64 {
        guest_call(&self.rt, self.at(off), args, &[]).unwrap_or_else(|f| panic!("guest stopped: {f:?}")).x0
    }

    fn api(&self) -> u64 {
        self.api.as_ptr() as u64
    }
}

/// One thread: write, protect, call, unprotect, rewrite, protect, call.
#[test]
fn code_rewritten_in_place_runs_the_new_code() {
    let w = World::new();
    for i in 0..100u64 {
        let (a, b) = (0x100 + i, 0x200 + i);
        let r = w.call(guest::GUEST_CODE_REWRITE, &[w.api(), a, b]);
        assert_eq!(r, (a << 16) | b, "round {i}: got {:#x}", r);
    }
}

/// One thread: write, protect, call, unmap, map again at the same address,
/// write, protect, call.
#[test]
fn code_unmapped_and_mapped_again_runs_the_new_code() {
    let w = World::new();
    for i in 0..100u64 {
        let (a, b) = (0x300 + i, 0x400 + i);
        let r = w.call(guest::GUEST_CODE_REMAP, &[w.api(), a, b]);
        assert_eq!(r, (a << 16) | b, "round {i}: got {:#x}", r);
    }
}

/// How a second thread waits between its two calls of the generated code.
#[derive(Clone, Copy, Debug)]
enum Wait {
    /// Spinning in translated code, never leaving the Jit.
    Spin,
    /// Inside a host call.
    Host,
}

/// How the first thread changes the code under the second.
#[derive(Clone, Copy, Debug)]
enum Change {
    Rewrite,
    Remap,
}

/// A second thread translates and runs the code, then waits; this thread
/// changes it; the second thread runs it again and must see the change.
fn across_threads(wait: Wait, change: Change) {
    let w = World::new();
    for i in 0..50u64 {
        let (a, b) = (0x500 + i, 0x600 + i);
        let f = w.call(guest::GUEST_CODE_MAP, &[w.api(), a]);
        assert_ne!(f, 0, "mapping failed");
        let flags: Arc<[AtomicI64; 2]> = Arc::new([AtomicI64::new(0), AtomicI64::new(0)]);
        let other = std::thread::spawn({
            let (rt, flags) = (w.rt.clone(), flags.clone());
            let pc = match wait {
                Wait::Spin => w.at(guest::GUEST_CODE_CALL_SPIN_CALL),
                Wait::Host => w.at(guest::GUEST_CODE_CALL_WAIT_CALL),
            };
            let args = [f, flags.as_ptr() as u64, w.wait];
            move || guest_call(&rt, pc, &args, &[]).map(|r| r.x0)
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        while flags[0].load(Ordering::Acquire) == 0 {
            assert!(Instant::now() < deadline, "the other thread never ran the code");
            std::thread::yield_now();
        }
        assert_eq!(flags[0].load(Ordering::Acquire) as u64, a);
        let r = match change {
            Change::Rewrite => w.call(guest::GUEST_CODE_PATCH, &[w.api(), f, b]),
            Change::Remap => w.call(guest::GUEST_CODE_REPLACE, &[w.api(), f, b]),
        };
        assert_eq!(r, 0, "changing the code failed at step {}", r as i64);
        // And this thread's own Jit, which has not run it, agrees.
        assert_eq!(w.call(guest::GUEST_CODE_CALL, &[f]), b);
        flags[1].store(1, Ordering::Release);
        let second = other.join().unwrap().unwrap_or_else(|f| panic!("other thread stopped: {f:?}"));
        assert_eq!(second, b, "round {i} ({wait:?}, {change:?}): the other thread ran {second:#x}, the old code");
        code::munmap(f, 4096, 0).unwrap().unwrap();
    }
}

#[test]
fn code_rewritten_under_a_spinning_thread() {
    across_threads(Wait::Spin, Change::Rewrite);
}

#[test]
fn code_rewritten_under_a_thread_in_a_host_call() {
    across_threads(Wait::Host, Change::Rewrite);
}

#[test]
fn code_remapped_under_a_spinning_thread() {
    across_threads(Wait::Spin, Change::Remap);
}

#[test]
fn code_remapped_under_a_thread_in_a_host_call() {
    across_threads(Wait::Host, Change::Remap);
}
