//! Guest memory the guest made executable, and the translations made of it.
//!
//! Guest memory is host memory (design §2), so the guest's `mmap`, `munmap`
//! and `mprotect` are the host's -- the flag and protection numbers are the
//! generic ones on both kernels. What the translator adds is a cache of
//! translated code keyed by guest address, one per Jit, and there are 60 and
//! more Jits in game (design §4). Guest code the engine generates into a
//! mapping it made executable, then unmaps or re-protects, leaves those
//! translations behind in every Jit that ran it; a later mapping at the same
//! address would run the old code. So every call here that takes executable
//! permission away from a range, unmaps it, maps over it or makes it
//! writable drops every Jit's translations of that range before it returns
//! to the guest (`jit::invalidate_everywhere` says how and why that is
//! enough), and so does one that makes a range executable, for code
//! translated from it while it was not.
//!
//! Until this existed the first such call stopped the client by name. It
//! came on leaving a game (the Quest build 2.740.927, over WiVRn): a guest
//! thread's `mprotect` of a range it had made executable.
//!
//! PROT_EXEC itself is withheld from the host mapping, as patches/0006 does
//! for the guest's text: the translator only reads guest code, and a stray
//! host jump into arm64 bytes then faults at once.
//!
//! One lock is held across each call, the host's answer and the
//! invalidation together. Without it a second thread's `mmap` could be given
//! an address this thread has just unmapped, and write and run new code
//! there, before this thread had dropped the old translations of it --
//! which on the real kernel cannot happen, since `munmap` has finished its
//! TLB shootdown before the address is free again.

use std::collections::BTreeMap;
use std::ffi::{c_int, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::jit::{invalidate_everywhere, Fault};

const PROT_WRITE: u64 = 2;
const PROT_EXEC: u64 = 4;
const MAP_FIXED: u64 = 0x10;
const PAGE: u64 = 4096;
const MADV_DONTNEED: c_int = 4;
const MADV_FREE: c_int = 8;
const MADV_REMOVE: c_int = 9;

/// The ranges the guest has made executable, `start -> end`, disjoint and
/// merged. Its lock is the one held across each call.
static EXEC: Mutex<BTreeMap<u64, u64>> = Mutex::new(BTreeMap::new());

unsafe extern "C" {
    #[link_name = "mmap"]
    fn host_mmap(a: *mut c_void, len: usize, prot: c_int, flags: c_int, fd: c_int, off: i64) -> *mut c_void;
    #[link_name = "munmap"]
    fn host_munmap(a: *mut c_void, len: usize) -> c_int;
    #[link_name = "mprotect"]
    fn host_mprotect(a: *mut c_void, len: usize, prot: c_int) -> c_int;
    #[link_name = "madvise"]
    fn host_madvise(a: *mut c_void, len: usize, advice: c_int) -> c_int;
    fn __errno_location() -> *mut c_int;
    fn gettid() -> i32;
}

fn errno() -> c_int {
    // SAFETY: the calling thread's own errno.
    unsafe { *__errno_location() }
}

/// `CORDIAL_GUEST_TRACE_EXEC=1`: one line on stderr for every call here
/// that makes a range executable or touches one that is, and for the guest's
/// instruction cache operations.
fn trace() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("CORDIAL_GUEST_TRACE_EXEC").is_some())
}

fn prot_str(prot: u64) -> String {
    let mut s = String::new();
    s.push(if prot & 1 != 0 { 'R' } else { '-' });
    s.push(if prot & 2 != 0 { 'W' } else { '-' });
    s.push(if prot & 4 != 0 { 'X' } else { '-' });
    s
}

fn page_end(a: u64, len: u64) -> u64 {
    a.saturating_add(len).saturating_add(PAGE - 1) & !(PAGE - 1)
}

/// The parts of `[a, end)` the guest has made executable.
fn overlaps(map: &BTreeMap<u64, u64>, a: u64, end: u64) -> Vec<(u64, u64)> {
    map.range(..end)
        .filter(|&(_, &e)| e > a)
        .map(|(&s, &e)| (s.max(a), e.min(end)))
        .collect()
}

fn remove(map: &mut BTreeMap<u64, u64>, a: u64, end: u64) {
    for (s, e) in overlaps(map, a, end) {
        // `s` is either a key or `a` inside the entry starting below it.
        let (&key, &kend) = map.range(..=s).next_back().expect("an overlap has an entry");
        map.remove(&key);
        if key < s {
            map.insert(key, s);
        }
        if e < kend {
            map.insert(e, kend);
        }
    }
}

fn add(map: &mut BTreeMap<u64, u64>, mut a: u64, mut end: u64) {
    let touching: Vec<(u64, u64)> = map.range(..=end).filter(|&(_, &e)| e >= a).map(|(&s, &e)| (s, e)).collect();
    for (s, e) in touching {
        map.remove(&s);
        a = a.min(s);
        end = end.max(e);
    }
    map.insert(a, end);
}

fn invalidate(name: &str, hit: &[(u64, u64)]) -> Result<(), Fault> {
    let done = invalidate_everywhere(hit).map_err(|why| Fault::Unsupported { thunk: name.into(), why })?;
    if trace() && !hit.is_empty() {
        // SAFETY: plain libc call.
        let tid = unsafe { gettid() };
        eprintln!("[exec] {tid} {name}: dropped {hit:x?} in {} Jits, waited {:?} for {} running",
                  done.jits, done.waited, done.running);
    }
    Ok(())
}

fn log(site: u64, what: std::fmt::Arguments) {
    // SAFETY: plain libc call.
    let tid = unsafe { gettid() };
    eprintln!("[exec] {tid} {what} from {site:#x}");
}

/// `mmap`, for the libc import and the raw `svc #0` path: the value, or the
/// errno. `site` is the guest address it was called from, for the trace.
pub fn mmap(a: u64, len: u64, prot: u64, flags: u64, fd: u64, off: u64, site: u64)
            -> Result<Result<u64, c_int>, Fault> {
    let prot = prot as u32 as u64;
    let mut exec = EXEC.lock().unwrap();
    // SAFETY: the guest's own mapping request.
    let p = unsafe {
        host_mmap(a as *mut c_void, len as usize, (prot & !PROT_EXEC) as c_int, flags as c_int,
                  fd as c_int, off as i64)
    } as u64;
    let failed = p == u64::MAX;
    let err = errno();
    // MAP_FIXED replaces whatever was there, and a failed MAP_FIXED may
    // already have unmapped it.
    let replaced = if flags & MAP_FIXED != 0 { overlaps(&exec, a, page_end(a, len)) } else { Vec::new() };
    if trace() && (prot & PROT_EXEC != 0 || !replaced.is_empty()) {
        log(site, format_args!("mmap {a:#x} len {len:#x} {} flags {flags:#x} fd {} -> {}{}",
                               prot_str(prot), fd as i32,
                               if failed { format!("errno {err}") } else { format!("{p:#x}") },
                               if replaced.is_empty() { String::new() } else { format!(", replacing executable {replaced:x?}") }));
    }
    if failed {
        invalidate("mmap", &replaced)?;
        return Ok(Err(err));
    }
    let end = page_end(p, len);
    // Anything still recorded where the kernel placed this was unmapped
    // without passing through here (a host `munmap` of guest memory).
    let mut hit = replaced;
    hit.extend(overlaps(&exec, p, end));
    remove(&mut exec, p, end);
    if prot & PROT_EXEC != 0 {
        add(&mut exec, p, end);
        hit.push((p, end));
    }
    invalidate("mmap", &hit)?;
    Ok(Ok(p))
}

pub fn munmap(a: u64, len: u64, site: u64) -> Result<Result<u64, c_int>, Fault> {
    let mut exec = EXEC.lock().unwrap();
    // SAFETY: the guest's own range.
    let r = unsafe { host_munmap(a as *mut c_void, len as usize) };
    let err = errno();
    let end = page_end(a, len);
    let hit = overlaps(&exec, a, end);
    if trace() && !hit.is_empty() {
        log(site, format_args!("munmap {a:#x} len {len:#x} -> {r}, executable {hit:x?}"));
    }
    if hit.is_empty() {
        return Ok(if r == 0 { Ok(0) } else { Err(err) });
    }
    if r == 0 {
        remove(&mut exec, a, end);
    }
    invalidate("munmap", &hit)?;
    Ok(if r == 0 { Ok(0) } else { Err(err) })
}

pub fn mprotect(a: u64, len: u64, prot: u64, site: u64) -> Result<Result<u64, c_int>, Fault> {
    let prot = prot as u32 as u64;
    let mut exec = EXEC.lock().unwrap();
    // SAFETY: the guest's own range.
    let r = unsafe { host_mprotect(a as *mut c_void, len as usize, (prot & !PROT_EXEC) as c_int) };
    let err = errno();
    let end = page_end(a, len);
    let was = overlaps(&exec, a, end);
    if trace() && (prot & PROT_EXEC != 0 || !was.is_empty()) {
        log(site, format_args!("mprotect {a:#x} len {len:#x} {} -> {r}{}", prot_str(prot),
                               if was.is_empty() { String::new() } else { format!(", was executable {was:x?}") }));
    }
    // Executable to executable and never writable is the one change that
    // cannot alter what is there or whether it may run.
    let hit = if prot & PROT_EXEC != 0 && prot & PROT_WRITE == 0 && was == [(a, end)] {
        Vec::new()
    } else if prot & PROT_EXEC != 0 {
        vec![(a, end)]
    } else {
        was
    };
    if r == 0 {
        remove(&mut exec, a, end);
        if prot & PROT_EXEC != 0 {
            add(&mut exec, a, end);
        }
    }
    // A failed mprotect may have changed part of the range (ENOMEM), so the
    // invalidation does not depend on the answer.
    invalidate("mprotect", &hit)?;
    Ok(if r == 0 { Ok(0) } else { Err(err) })
}

/// `madvise`: the advice that discards contents (`MADV_DONTNEED`, `FREE`,
/// `REMOVE`) changes the code in an executable range as surely as writing it.
pub fn madvise(a: u64, len: u64, advice: c_int, site: u64) -> Result<Result<u64, c_int>, Fault> {
    let exec = EXEC.lock().unwrap();
    // SAFETY: the guest's own range.
    let r = unsafe { host_madvise(a as *mut c_void, len as usize, advice) };
    let err = errno();
    let hit = if matches!(advice, MADV_DONTNEED | MADV_FREE | MADV_REMOVE) {
        overlaps(&exec, a, page_end(a, len))
    } else {
        Vec::new()
    };
    if trace() && !hit.is_empty() {
        log(site, format_args!("madvise {a:#x} len {len:#x} advice {advice} -> {r}, executable {hit:x?}"));
    }
    invalidate("madvise", &hit)?;
    Ok(if r == 0 { Ok(0) } else { Err(err) })
}

static ICACHE_SEEN: AtomicBool = AtomicBool::new(false);

pub(crate) fn on_icache(op: u32, va: u64) {
    if trace() && !ICACHE_SEEN.swap(true, Ordering::Relaxed) {
        // SAFETY: plain libc call.
        let tid = unsafe { gettid() };
        eprintln!("[exec] {tid} first instruction cache operation: op {op} va {va:#x}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_split_and_merge() {
        let mut m = BTreeMap::new();
        add(&mut m, 0x1000, 0x5000);
        add(&mut m, 0x5000, 0x6000);
        assert_eq!(m.iter().map(|(&a, &b)| (a, b)).collect::<Vec<_>>(), [(0x1000, 0x6000)]);
        remove(&mut m, 0x2000, 0x3000);
        assert_eq!(m.iter().map(|(&a, &b)| (a, b)).collect::<Vec<_>>(), [(0x1000, 0x2000), (0x3000, 0x6000)]);
        assert_eq!(overlaps(&m, 0x1800, 0x4000), [(0x1800, 0x2000), (0x3000, 0x4000)]);
        remove(&mut m, 0, 0x10000);
        assert!(m.is_empty());
    }
}
