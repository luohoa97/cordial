//! Thunks the generic call builder cannot express: variadic, and
//! callback-taking (design §3.1, "Hand-written").
//!
//! Each returns a `Handler` to register under the import's name. None of them
//! reports a success it did not have: what a thunk cannot do faithfully is
//! either an errno the real function could also have returned, or a `Fault`
//! naming the thunk, which stops the guest where the gap is.

use std::ffi::{c_void, CStr};
use std::sync::Arc;

use crate::abi::{printf_types, Ret, Ty};
use crate::jit::{guest_call, Fault, Handler, Runtime};

const EINVAL: u64 = 22;

unsafe extern "C" {
    fn qsort_r(
        base: *mut c_void,
        n: usize,
        size: usize,
        cmp: extern "C" fn(*const c_void, *const c_void, *mut c_void) -> i32,
        arg: *mut c_void,
    );
    fn pthread_create(
        t: *mut u64,
        attr: *const c_void,
        start: extern "C" fn(*mut c_void) -> *mut c_void,
        arg: *mut c_void,
    ) -> i32;
}

/// A printf-family function: `named` fixed arguments, the format string at
/// `fmt_index` among them, then whatever the format consumes.
///
/// The host is glibc and the guest expects bionic, and the two do not format
/// identically everywhere (`%p` of NULL is `(nil)` in one and `0x0` in the
/// other). That is a difference of libc, not of ABI, and is left for the
/// libc thunk layer to decide; this only moves the arguments.
pub fn printf_like(name: &'static str, f: *const c_void, named: &'static [Ty], fmt_index: usize) -> Handler {
    let f = f as usize;
    Box::new(move |c| {
        // SAFETY: the guest passed a C string; identity mapping makes its
        // address a host address.
        let fmt = unsafe { CStr::from_ptr(c.x(fmt_index as u32) as *const i8) };
        let rest = printf_types(fmt.to_bytes())
            .map_err(|why| Fault::Unsupported { thunk: name.into(), why })?;
        let mut all = named.to_vec();
        all.extend(rest);
        c.host(f as *const c_void, &all, Ret::Int(Ty::I32))
    })
}

/// `qsort(base, n, size, cmp)` with `cmp` a guest function.
///
/// The host's `qsort_r` calls back into a host comparator, which calls the
/// guest's through `guest_call` -- one re-entry deeper, on the next Jit. A
/// guest fault inside the comparator cannot unwind through glibc's frames,
/// so it is parked, the remaining comparisons answer "equal" without
/// entering the guest again, and the fault is raised once `qsort_r` has
/// returned.
pub fn qsort() -> Handler {
    struct Ctx<'a> {
        rt: &'a Arc<Runtime>,
        cmp: u64,
        fault: Option<Fault>,
    }
    unsafe extern "C" fn host_cmp(a: *const c_void, b: *const c_void, ctx: *mut c_void) -> i32 {
        // SAFETY: `ctx` is the Ctx below, live for the whole qsort_r call.
        let ctx = unsafe { &mut *(ctx as *mut Ctx) };
        if ctx.fault.is_some() {
            return 0;
        }
        match guest_call(ctx.rt, ctx.cmp, &[a as u64, b as u64], &[]) {
            Ok(r) => r.x0 as i32,
            Err(f) => {
                ctx.fault = Some(f);
                0
            }
        }
    }
    Box::new(|c| {
        let mut ctx = Ctx { rt: c.runtime(), cmp: c.x(3), fault: None };
        // SAFETY: base/n/size are the guest's, and describe guest memory the
        // host can address directly; the comparator is ours.
        unsafe {
            qsort_r(c.x(0) as *mut c_void, c.x(1) as usize, c.x(2) as usize, host_cmp,
                    &mut ctx as *mut Ctx as *mut c_void)
        };
        match ctx.fault {
            Some(f) => Err(f),
            None => Ok(()),
        }
    })
}

/// `pthread_create(thread, attr, start, arg)` with `start` a guest function.
///
/// The new host thread gets its own guest stack, TLS block and Jit the first
/// time it enters the guest, which is immediately. A non-null `attr` is
/// refused with EINVAL: bionic's `pthread_attr_t` is not glibc's, and a
/// requested stack size or detach state that was silently dropped would be
/// a success that was not one. Reading bionic's layout is M4's work.
pub fn pthread_create_thunk() -> Handler {
    struct Start {
        rt: Arc<Runtime>,
        pc: u64,
        arg: u64,
    }
    unsafe extern "C" fn start(p: *mut c_void) -> *mut c_void {
        // SAFETY: the Box leaked by the handler below, handed over once.
        let s = unsafe { Box::from_raw(p as *mut Start) };
        match guest_call(&s.rt, s.pc, &[s.arg], &[]) {
            Ok(r) => r.x0 as *mut c_void,
            // A guest thread has nobody to return a fault to. On Android the
            // process would take a signal and die; this does the same, named.
            Err(f) => {
                eprintln!("cordial-guest: guest thread at {:#x} faulted: {f:?}", s.pc);
                std::process::abort();
            }
        }
    }
    Box::new(|c| {
        if c.x(1) != 0 {
            c.set_x(0, EINVAL);
            return Ok(());
        }
        let s = Box::into_raw(Box::new(Start { rt: c.runtime().clone(), pc: c.x(2), arg: c.x(3) }));
        let mut t = 0u64;
        // SAFETY: `start` takes ownership of `s` if and only if creation
        // succeeds.
        let r = unsafe { pthread_create(&mut t, std::ptr::null(), start, s as *mut c_void) };
        if r == 0 {
            // SAFETY: the guest's pthread_t* is a host address; bionic's
            // pthread_t is a long, glibc's an unsigned long.
            unsafe { (c.x(0) as *mut u64).write(t) };
        } else {
            // SAFETY: not handed over, so still ours.
            drop(unsafe { Box::from_raw(s) });
        }
        c.set_x(0, r as u32 as u64);
        Ok(())
    })
}
