//! M0 of docs/vr/dynarmic-design.md §8: dynarmic builds, runs a guest
//! function, and stops at the return stub; an unallocated word faults.
//!
//! Hand-assembled words rather than the compiled image, so this milestone
//! depends on nothing but dynarmic and the shim. Encodings from
//! `llvm-mc -triple=aarch64 -mattr=+fullfp16,+lse -show-encoding`.
#![cfg(all(feature = "dynarmic", target_arch = "x86_64"))]

use cordial_guest::{guest_call, Exception, Fault, Mapping, Options, Runtime};

const RET: u32 = 0xd65f_03c0;

fn words(w: &[u32]) -> Mapping {
    let bytes: Vec<u8> = w.iter().flat_map(|w| w.to_le_bytes()).collect();
    Mapping::with_contents(&bytes, 0)
}

#[test]
fn m0_add_returns_through_the_ret_stub() {
    let rt = Runtime::new(Options::default());
    let code = words(&[0x8b01_0000, RET]); // add x0, x0, x1; ret
    let r = guest_call(&rt, code.addr(), &[2, 3], &[]).expect("guest returned");
    println!("M0 pass: add(2,3) = {} ; stopped at pc {:#x}, svc #RET stub at {:#x}",
             r.x0, r.pc, rt.ret_stub());
    assert_eq!(r.x0, 5);
    assert_eq!(r.pc, rt.ret_stub() + 4, "halted somewhere other than just past svc #RET");
}

/// The control as design §8 wrote it expected `ExceptionRaised(Unallocated
/// Encoding)`. It is not what this dynarmic does: `udf #0` has no decoder
/// entry, and an instruction with no entry goes to `InterpreterFallback`.
/// Either way it stops, at the right PC, having executed nothing.
#[test]
fn m0_control_zero_word_stops() {
    let rt = Runtime::new(Options::default());
    let code = words(&[0x0000_0000, RET]);
    let r = guest_call(&rt, code.addr(), &[2, 3], &[]);
    println!("M0 control: .word 0 at {:#x} -> {r:?}", code.addr());
    assert_eq!(r.unwrap_err(), Fault::InterpreterFallback { pc: code.addr(), count: 1 });
}

/// What does reach `ExceptionRaised(UnallocatedEncoding)`: an instruction
/// the decoder matches but whose operand combination this dynarmic treats
/// as unallocated -- here half-precision `scvtf`, which is FEAT_FP16.
#[test]
fn m0_control_fp16_is_unallocated_encoding() {
    let rt = Runtime::new(Options::default());
    let code = words(&[0x9ee2_0000, RET]); // scvtf h0, x0
    let r = guest_call(&rt, code.addr(), &[2], &[]);
    println!("M0 control: scvtf h0, x0 at {:#x} -> {r:?}", code.addr());
    assert_eq!(r.unwrap_err(),
               Fault::Exception { pc: code.addr(), kind: Exception::UnallocatedEncoding });
}

/// Design §1.3/§8 (M3's control) expect an LSE atomic to raise
/// `UnallocatedEncoding`. Its decoder lines are commented out, so it is an
/// `InterpreterFallback` too. Still a stop, still at the helper.
#[test]
fn m0_lse_atomic_stops_at_the_instruction() {
    let rt = Runtime::new(Options::default());
    let mut word = 0u64;
    let code = words(&[0xf8e1_0040, RET]); // ldaddal x1, x0, [x2]
    let r = guest_call(&rt, code.addr(), &[0, 1, &mut word as *mut u64 as u64], &[]);
    println!("LSE: ldaddal at {:#x} -> {r:?} (word still {word})", code.addr());
    assert_eq!(r.unwrap_err(), Fault::InterpreterFallback { pc: code.addr(), count: 1 });
    assert_eq!(word, 0);
}

#[test]
fn m0_unregistered_stub_slot_stops() {
    // A jump into a stub slot nobody registered must stop by name, not run.
    let rt = Runtime::new(Options::default());
    let unused = rt.ret_stub() - 16;
    let r = guest_call(&rt, unused, &[], &[]);
    println!("unregistered stub {unused:#x} -> {r:?}");
    assert!(matches!(r, Err(Fault::InterpreterFallback { pc, .. }) if pc == unused));
}
