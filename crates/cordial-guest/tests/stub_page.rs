//! The dispatch half of ADR-053's first condition, that the translator is
//! never keyed on an engine address: an SVC reaches a handler only from its
//! own stub, and the same instruction anywhere else in guest memory is the
//! guest's own system call, as arm64 Linux treats it.
//!
//! Hand-assembled, as in `m0.rs`. `svc #imm` is `0xd4000001 | imm << 5`.
#![cfg(target_arch = "x86_64")]
#![allow(unsafe_code)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use cordial_guest::{guest_call, Fault, Mapping, Options, Runtime};

const RET: u32 = 0xd65f_03c0;

fn svc(imm: u32) -> u32 {
    0xd400_0001 | (imm << 5)
}

fn words(w: &[u32]) -> Mapping {
    let bytes: Vec<u8> = w.iter().flat_map(|w| w.to_le_bytes()).collect();
    Mapping::with_contents(&bytes, 0)
}

/// A runtime with one registered stub whose handler counts its calls, and
/// that stub's SVC immediate, read back out of the stub itself.
fn counted() -> (Arc<Runtime>, u64, u32, Arc<AtomicU64>) {
    let rt = Runtime::new(Options::default());
    let hits = Arc::new(AtomicU64::new(0));
    let h = hits.clone();
    let stub = rt.register("counted", Box::new(move |_| {
        h.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }));
    // SAFETY: the stub page is mapped readable for the runtime's lifetime.
    let first = unsafe { (stub as *const u32).read() };
    assert_eq!(first & 0xffe0_001f, 0xd400_0001, "the stub does not start with an svc");
    (rt, stub, (first >> 5) & 0xffff, hits)
}

#[test]
fn a_stubs_svc_copied_outside_the_page_reaches_no_handler() {
    let (rt, stub, imm, hits) = counted();

    // Control: entered at its own address, the stub reaches its handler.
    guest_call(&rt, stub, &[], &[]).expect("the stub returned");
    assert_eq!(hits.load(Ordering::Relaxed), 1);

    // The same instruction in guest memory of its own: with no syscall
    // handler installed it stops as a syscall, at the SVC, having run
    // nothing.
    let code = words(&[svc(imm), RET]);
    let r = guest_call(&rt, code.addr(), &[], &[]);
    println!("svc #{imm} outside the stub page at {:#x} -> {r:?}", code.addr());
    assert_eq!(r.unwrap_err(), Fault::Syscall { pc: code.addr() });
    assert_eq!(hits.load(Ordering::Relaxed), 1, "the stub's handler ran for an svc outside the page");
}

#[test]
fn an_svc_outside_the_page_is_the_guests_own_syscall_whatever_its_immediate() {
    let (rt, _stub, imm, hits) = counted();
    let syscalls = Arc::new(AtomicU64::new(0));
    let s = syscalls.clone();
    rt.set_syscall_handler(Box::new(move |c| {
        s.fetch_add(1, Ordering::Relaxed);
        c.set_x(0, 0);
        Ok(())
    }));
    // svc #0, the stub's own immediate, and the return stub's: each is a
    // syscall here, and the last does not end the call early.
    let code = words(&[svc(0), svc(imm), svc(0xffff), 0xd280_0540, RET]); // ...; mov x0, #42; ret
    let r = guest_call(&rt, code.addr(), &[], &[]).expect("the guest returned");
    assert_eq!(r.x0, 42, "svc #0xffff outside the page halted the call");
    assert_eq!(syscalls.load(Ordering::Relaxed), 3);
    assert_eq!(rt.syscall_count(), 3);
    assert_eq!(hits.load(Ordering::Relaxed), 0, "the stub's handler ran for an svc outside the page");
}
