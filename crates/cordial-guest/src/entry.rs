//! Host-to-guest entry points: a host function pointer that runs a guest
//! function (design §3.3).
//!
//! Needed wherever the host keeps a guest function to call later as though
//! it were its own -- a JNI native the engine hands to `RegisterNatives` is
//! the case at hand: libjnivm will call it with a SysV frame, and the address
//! it was given is arm64 code. Each entry is one 16-byte slot in a page of
//! x86-64 code built once, up front, and never written again: the slot loads
//! its index and jumps to `cg_host_entry_common`, which saves the SysV
//! argument registers, and `cg_host_entry_dispatch` below reads them by the
//! entry's type list and makes the guest call. The page is Cordial's own; no
//! guest byte is involved.

use std::ffi::c_void;
use std::sync::{Arc, OnceLock, RwLock};

use crate::abi::{Ret, Ty};
use crate::jit::{guest_call_args, last_fault_context, Runtime};
use crate::mem::{Mapping, PROT_READ};

/// Slots in the entry page. The engine registers a few hundred natives and
/// exports 526 `Java_*` functions; running out fails the request by name
/// rather than reusing a slot.
const SLOTS: usize = 8192;
const SLOT_BYTES: usize = 16;

unsafe extern "C" {
    fn cg_host_entry_common();
}

/// Maps the `i`th argument's host value to the guest's (the JNIEnv*).
pub type ArgMap = Box<dyn Fn(usize, u64) -> u64 + Send + Sync>;

struct Entry {
    rt: Arc<Runtime>,
    name: String,
    pc: u64,
    args: Vec<Ty>,
    ret: Ret,
    map: Option<ArgMap>,
}

struct Page {
    map: Mapping,
    entries: RwLock<Vec<Entry>>,
}

static PAGE: OnceLock<Page> = OnceLock::new();

unsafe extern "C" {
    fn mprotect(addr: *mut c_void, len: usize, prot: i32) -> i32;
}
const PROT_EXEC: i32 = 4;

fn page() -> &'static Page {
    PAGE.get_or_init(|| {
        let map = Mapping::new(SLOT_BYTES + SLOTS * SLOT_BYTES);
        let base = map.addr();
        // SAFETY: the mapping is fresh, writable and large enough; after the
        // mprotect below it is never written again.
        unsafe {
            (base as *mut u64).write(cg_host_entry_common as *const () as u64);
            for i in 0..SLOTS {
                let slot = base + (SLOT_BYTES + i * SLOT_BYTES) as u64;
                let p = slot as *mut u8;
                // mov $i, %r10d
                p.write(0x41);
                p.add(1).write(0xba);
                (p.add(2) as *mut u32).write_unaligned(i as u32);
                // jmp *disp32(%rip), to the qword at the page's start
                p.add(6).write(0xff);
                p.add(7).write(0x25);
                let disp = base as i64 - (slot as i64 + 12);
                (p.add(8) as *mut i32).write_unaligned(disp as i32);
                for k in 12..16 {
                    p.add(k).write(0xcc);
                }
            }
            let r = mprotect(base as *mut c_void, map.len(), PROT_READ | PROT_EXEC);
            assert_eq!(r, 0, "mprotect of the host entry page failed");
        }
        Page { map, entries: RwLock::new(Vec::new()) }
    })
}

/// A host function pointer that calls the guest function at `pc`, whose
/// arguments are typed by `args` in SysV terms on the host side and AAPCS64
/// on the guest's. `map` rewrites an argument on the way in.
pub fn host_entry(rt: &Arc<Runtime>, name: &str, pc: u64, args: Vec<Ty>, ret: Ret, map: Option<ArgMap>)
    -> Result<*const c_void, String>
{
    let p = page();
    let mut e = p.entries.write().unwrap();
    if e.len() >= SLOTS {
        return Err(format!("host entry page full ({SLOTS} slots) at {name}"));
    }
    let i = e.len();
    e.push(Entry { rt: rt.clone(), name: name.to_owned(), pc, args, ret, map });
    Ok((p.map.addr() + (SLOT_BYTES + i * SLOT_BYTES) as u64) as *const c_void)
}

/// How many host entries exist.
pub fn host_entry_count() -> usize {
    PAGE.get().map_or(0, |p| p.entries.read().unwrap().len())
}

#[repr(C)]
struct EntryRegs {
    gpr: [u64; 6],
    xmm: [[u64; 2]; 8],
    stack: u64,
    rax: u64,
    xmm0: [u64; 2],
}

#[no_mangle]
unsafe extern "C" fn cg_host_entry_dispatch(index: u64, regs: *mut EntryRegs) {
    // SAFETY: `regs` is cg_host_entry_common's frame, live for this call.
    let regs = unsafe { &mut *regs };
    let page = page();
    let entries = page.entries.read().unwrap();
    let e = &entries[index as usize];
    let (mut ni, mut nv, mut ns) = (0usize, 0usize, 0u64);
    let mut vals = Vec::with_capacity(e.args.len());
    for (i, &ty) in e.args.iter().enumerate() {
        let raw = if ty.is_fp() && nv < 8 {
            nv += 1;
            regs.xmm[nv - 1][0]
        } else if !ty.is_fp() && ni < 6 {
            ni += 1;
            regs.gpr[ni - 1]
        } else {
            ns += 1;
            // SAFETY: the caller's stack arguments, above its return address.
            unsafe { ((regs.stack + 8 * (ns - 1)) as *const u64).read() }
        };
        let v = match &e.map {
            Some(m) => m(i, raw),
            None => raw,
        };
        vals.push((ty, v));
    }
    let (rt, pc, ret, name) = (e.rt.clone(), e.pc, e.ret, e.name.clone());
    drop(entries);
    match guest_call_args(&rt, pc, &vals) {
        Ok(r) => match ret {
            Ret::Void => {}
            Ret::Int(t) => regs.rax = t.extend(r.x0),
            Ret::F64 => regs.xmm0 = [r.v0[0], 0],
            Ret::F32 => regs.xmm0 = [r.v0[0] & 0xffff_ffff, 0],
        },
        // The host caller has no way to receive a guest fault. On Android
        // a native that faults takes the process with it; this does the
        // same, and says where.
        Err(f) => {
            eprintln!("cordial-guest: guest function {name} at {pc:#x}, called by the host, stopped: {f:?}");
            for c in last_fault_context() {
                eprintln!("  guest pc {:#x} lr {:#x} sp {:#x} frames {:x?}", c.pc, c.lr, c.sp, c.frames);
            }
            std::process::abort();
        }
    }
}
