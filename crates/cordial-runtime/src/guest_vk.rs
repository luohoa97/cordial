//! The arm64 guest's `libvulkan.so` (docs/vr/dynarmic-design.md §3.2, M5).
//!
//! The Quest build ships only Vulkan shader packs, so under dynarmic Vulkan is
//! the only renderer it can draw with (§9.3). It reaches Vulkan exactly as the
//! phone build does: `dlopen("libvulkan.so")`, `dlsym("vkGetInstanceProcAddr")`,
//! and everything else through that. So the guest library has one export,
//! as Cordial's native virtual library does (`android::vulkan`), and every
//! pointer `vkGetInstanceProcAddr`/`vkGetDeviceProcAddr` hands back is a stub
//! made for that command from its generated signature
//! (`tools/vr/gen-guest-vk.py`, from Khronos's `vk.xml`), made once per
//! (name, host function) -- the device-level pointers differ per device.
//!
//! **Behind every stub is what the native engine would have called**: the
//! host pointer comes from Cordial's own `vkGetInstanceProcAddr`, so
//! `VK_KHR_android_surface`'s translation to the host's surface extension,
//! the swapchain's present mode and the capture behind `cordial_screenshot`
//! all stay where they are, and everything else is the host loader and ICD.
//!
//! Plain-data structs cross unchanged: the layout-diff gate
//! (`tests::layout_gate`, over `guest_vk_probe.c`) shows every struct and union
//! in `vulkan_core.h`, `vulkan_android.h` and `vulkan_wayland.h` identical in
//! size, alignment, member offsets and bitfield placement on aarch64-android
//! and x86-64 Linux. What cannot cross is a function pointer the host would
//! call: the guest's is arm64 code. Those are
//!
//! * `VkAllocationCallbacks`, whose five functions are each put behind a
//!   host-to-guest entry. The specification does not let an implementation
//!   ignore a `pAllocator` it was given, so it is translated, never dropped;
//! * a debug-report, debug-utils or device-memory-report callback in a create
//!   info or anywhere in its `pNext` chain. The chain is copied up to the last
//!   such link, each copy relinked to the next, the callback replaced by an
//!   entry, and the rest of the guest's chain left where it is. The guest's
//!   own structs are never written;
//! * `VkDirectDriverLoadingListLUNARG`, which hands the loader a
//!   `vkGetInstanceProcAddr` to call, and is refused by name: there is no
//!   guest driver to load.

use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr};
use std::sync::{Arc, Mutex, OnceLock};

use cordial_guest::{Fault, Handler, Ret, Runtime, Ty};

#[path = "guest_vk_table.rs"]
#[allow(dead_code)]
mod table;

/// Commands by name: argument types and return.
fn signatures() -> &'static HashMap<&'static str, (&'static [Ty], Ret)> {
    static S: OnceLock<HashMap<&'static str, (&'static [Ty], Ret)>> = OnceLock::new();
    S.get_or_init(|| table::VK.iter().map(|&(n, a, r)| (n, (a, r))).collect())
}

fn callback_sig(pfn: &str) -> Option<(&'static [Ty], Ret)> {
    table::CALLBACKS.iter().find(|(n, _, _)| *n == pfn).map(|&(_, a, r)| (a, r))
}

fn unsupported(thunk: &str, why: String) -> Fault {
    Fault::Unsupported { thunk: thunk.to_owned(), why }
}

/// The guest's `vkGetInstanceProcAddr`, over Cordial's native one.
pub fn get_instance_proc_addr(native: usize) -> Handler {
    let _ = NATIVE_GIPA.set(native);
    proc_addr("vkGetInstanceProcAddr", native)
}

static NATIVE_GIPA: OnceLock<usize> = OnceLock::new();

/// The host function behind a guest `vkGetInstanceProcAddr` pointer, if that
/// pointer is one of this layer's own stubs for it: Cordial's native getter.
/// This is how `XR_KHR_vulkan_enable2`'s `pfnGetInstanceProcAddr`, which the
/// engine fills with what `dlsym` gave it, reaches the runtime as something
/// the runtime can call (`guest_xr`). Anything else is arm64 code, and `None`.
pub(crate) fn host_gipa_for(rt: &Runtime, guest: u64) -> Option<usize> {
    match rt.stub_name(guest).as_deref() {
        Some("vkGetInstanceProcAddr") => NATIVE_GIPA.get().copied(),
        _ => None,
    }
}

/// `vkGet{Instance,Device}ProcAddr(handle, name)`: the host's answer, handed
/// back as a stub. A name the host does not have is null either way; one the
/// host has but the table cannot describe is null too, said once, since the
/// guest would otherwise jump to x86 code.
fn proc_addr(which: &'static str, host: usize) -> Handler {
    Box::new(move |c| {
        let name_ptr = c.x(1) as *const c_char;
        if name_ptr.is_null() {
            c.set_x(0, 0);
            return Ok(());
        }
        // SAFETY: the host function is Cordial's or the loader's
        // `PFN_vkVoidFunction (*)(handle, const char*)`, called with the
        // guest's own handle and C string; identity mapping.
        let out = unsafe { cordial_guest::invoke(which, host as *const c_void, &[Ty::Ptr, Ty::Ptr], &[c.x(0), c.x(1)]) }?;
        // SAFETY: the guest's NUL-terminated command name.
        let name = unsafe { CStr::from_ptr(name_ptr) }.to_string_lossy();
        let stub = wrap(c.runtime(), &name, out.rax as usize)?;
        c.set_x(0, stub);
        Ok(())
    })
}

/// The guest stub for host function `host` implementing command `name`.
fn wrap(rt: &Arc<Runtime>, name: &str, host: usize) -> Result<u64, Fault> {
    static MADE: Mutex<Option<HashMap<(String, usize), u64>>> = Mutex::new(None);
    static REFUSED: Mutex<Option<std::collections::HashSet<String>>> = Mutex::new(None);
    if host == 0 {
        return Ok(0);
    }
    let key = (name.to_owned(), host);
    if let Some(&s) = MADE.lock().unwrap().get_or_insert_with(HashMap::new).get(&key) {
        return Ok(s);
    }
    let Some((&static_name, &(args, ret))) = signatures().get_key_value(name) else {
        if REFUSED.lock().unwrap().get_or_insert_with(Default::default).insert(name.to_owned()) {
            let why = table::REFUSED.iter().find(|(n, _)| *n == name).map_or("not in the generated table", |(_, w)| w);
            eprintln!("[guest] vulkan: {name}: the host has it, but {why}; answered null");
        }
        return Ok(0);
    };
    let handler = match static_name {
        "vkGetInstanceProcAddr" | "vkGetDeviceProcAddr" => proc_addr(static_name, host),
        _ => command(static_name, host, args, ret),
    };
    let stub = rt.register(static_name, handler);
    MADE.lock().unwrap().get_or_insert_with(HashMap::new).insert(key, stub);
    Ok(stub)
}

/// One command's stub handler: the generic call, with a `pAllocator` or a
/// callback-carrying create info translated first where the command has one,
/// and the handle kinds [`live`] follows tracked around it.
fn command(name: &'static str, host: usize, args: &'static [Ty], ret: Ret) -> Handler {
    let inner = call(name, host, args, ret);
    match live::kind_of(name) {
        Some(k) => live::tracked(name, k, inner),
        None => inner,
    }
}

fn call(name: &'static str, host: usize, args: &'static [Ty], ret: Ret) -> Handler {
    let alloc = table::ALLOCATOR_ARG.iter().find(|(n, _)| *n == name).map(|&(_, i)| i);
    let chain = table::CHAIN_ARGS.iter().find(|(n, _, _, _)| *n == name).map(|&(_, i, _, arr)| (i, arr));
    if let Some(at) = xr_image_arg(name) {
        return Box::new(move |c| {
            note_xr_image_use(name, c, at);
            note_format(name, c);
            c.host(host as *const c_void, args, ret)
        });
    }
    if matches!(name, "vkCreateImage" | "vkCreateSwapchainKHR") {
        return Box::new(move |c| {
            note_format(name, c);
            c.host(host as *const c_void, args, ret)
        });
    }
    if alloc.is_none() && chain.is_none() {
        return Box::new(move |c| c.host(host as *const c_void, args, ret));
    }
    Box::new(move |c| {
        let mut vals = cordial_guest::collect(c, args);
        let mut keep: Vec<Box<[u64]>> = Vec::new();
        if let Some(i) = alloc {
            vals[i] = allocator(c.runtime(), vals[i])?;
        }
        if let Some((i, arr)) = chain {
            if arr {
                return Err(unsupported(name, "an array of create infos that may carry callbacks".into()));
            }
            // SAFETY: the guest's `const T*` argument, identity-mapped.
            vals[i] = unsafe { translate_chain(c.runtime(), name, vals[i], &mut keep) }?;
        }
        // SAFETY: the host function for `name`, whose SysV signature `args`
        // describes, with the guest's values and host copies that live in
        // `keep` for the length of the call.
        let out = unsafe { cordial_guest::invoke(name, host as *const c_void, args, &vals) }?;
        cordial_guest::write_ret(c, ret, &out);
        drop(keep);
        Ok(())
    })
}

/// Says, once per command and format, which formats the engine creates
/// images, views and window swapchains in -- whether it ever asks the
/// hardware for an sRGB encode or decode, which is what says how the bytes it
/// writes are encoded (design §9.7).
fn note_format(name: &'static str, c: &cordial_guest::Call) {
    static SAID: Mutex<Option<std::collections::HashSet<(&'static str, i32)>>> = Mutex::new(None);
    // Offset of the format in the create info: VkImageCreateInfo 24,
    // VkImageViewCreateInfo and VkSwapchainCreateInfoKHR 36.
    let at = match name {
        "vkCreateImage" => 24,
        "vkCreateImageView" | "vkCreateSwapchainKHR" => 36,
        _ => return,
    };
    let info = c.x(1);
    if info == 0 {
        return;
    }
    // SAFETY: the guest's create info, whose layout the gate checked.
    let f = unsafe { *((info + at) as *const i32) };
    if SAID.lock().unwrap().get_or_insert_with(Default::default).insert((name, f)) {
        eprintln!("[guest] vulkan: {name} format {f} (first of that format)");
    }
}

/// Where a command names an image the engine may write or view, for
/// `note_xr_image_use`: the argument index, or `usize::MAX` for
/// `vkCreateImageView`, whose image is inside its create info.
fn xr_image_arg(name: &str) -> Option<usize> {
    Some(match name {
        "vkCreateImageView" => usize::MAX,
        "vkCmdBlitImage" | "vkCmdResolveImage" | "vkCmdCopyImage" => 3,
        "vkCmdClearColorImage" => 1,
        "vkCmdCopyBufferToImage" => 2,
        _ => return None,
    })
}

/// Says, once per command and format pair, when the engine views or writes
/// an OpenXR swapchain image, and in what format: what decides whether the
/// sRGB swapchain `guest_xr::create_swapchain` makes keeps the engine's bytes
/// unchanged. A view in the image's own format or a UNORM view of an sRGB
/// image, and a copy, leave bytes as written; a blit, resolve or clear into
/// an sRGB image would encode them.
fn note_xr_image_use(name: &'static str, c: &cordial_guest::Call, at: usize) {
    static SAID: Mutex<Option<std::collections::HashSet<(&'static str, i64, i64)>>> = Mutex::new(None);
    let (image, viewed) = if at == usize::MAX {
        let info = c.x(1);
        if info == 0 {
            return;
        }
        // SAFETY: the guest's VkImageViewCreateInfo: image at 24, format at 36.
        unsafe { (*((info + 24) as *const u64), *((info + 36) as *const i32) as i64) }
    } else {
        (c.x(at as u32), -1)
    };
    let Some(made) = crate::guest_xr::swapchain_image_format(image) else {
        return;
    };
    if SAID.lock().unwrap().get_or_insert_with(Default::default).insert((name, made, viewed)) {
        if viewed >= 0 {
            eprintln!("[guest] vulkan: {name} on an OpenXR swapchain image (made as VkFormat {made}): view format {viewed}");
        } else {
            eprintln!("[guest] vulkan: {name} writes an OpenXR swapchain image (made as VkFormat {made})");
        }
    }
}

/// A host-callable entry running guest function `pc` as callback type `pfn`,
/// made once per (function, type).
fn callback_entry(rt: &Arc<Runtime>, pfn: &str, pc: u64) -> Result<u64, Fault> {
    static ENTRIES: Mutex<Option<HashMap<(u64, String), u64>>> = Mutex::new(None);
    if pc == 0 {
        return Ok(0);
    }
    let mut m = ENTRIES.lock().unwrap();
    let m = m.get_or_insert_with(HashMap::new);
    if let Some(&e) = m.get(&(pc, pfn.to_owned())) {
        return Ok(e);
    }
    let (args, ret) = callback_sig(pfn).ok_or_else(|| unsupported(pfn, "no generated signature".into()))?;
    let e = cordial_guest::host_entry(rt, pfn, pc, args.to_vec(), ret, None)
        .map_err(|why| unsupported(pfn, why))? as u64;
    m.insert((pc, pfn.to_owned()), e);
    Ok(e)
}

/// `const VkAllocationCallbacks*` from the guest: a host copy whose five
/// functions are host entries into the guest's, made once per distinct
/// struct content and kept for the process, since an object must be
/// destroyed with an allocator compatible with the one that created it.
pub(crate) fn allocator(rt: &Arc<Runtime>, guest: u64) -> Result<u64, Fault> {
    type Copies = HashMap<[u64; 6], Box<[u64; 6]>>;
    static COPIES: Mutex<Option<Copies>> = Mutex::new(None);
    if guest == 0 {
        return Ok(0);
    }
    // SAFETY: the guest's VkAllocationCallbacks: six pointer-sized fields,
    // the same 48 bytes on both sides (the gate checks it).
    let g = unsafe { (guest as *const [u64; 6]).read_unaligned() };
    let mut m = COPIES.lock().unwrap();
    let m = m.get_or_insert_with(HashMap::new);
    if let Some(h) = m.get(&g) {
        return Ok(&**h as *const [u64; 6] as u64);
    }
    let mut h = Box::new(g);
    for &(s, _, off, _, pfn) in table::PFN_MEMBERS.iter().filter(|r| r.0 == "VkAllocationCallbacks") {
        debug_assert_eq!(s, "VkAllocationCallbacks");
        h[off / 8] = callback_entry(rt, pfn, g[off / 8])?;
    }
    let p = &*h as *const [u64; 6] as u64;
    m.insert(g, h);
    Ok(p)
}

fn stype_size(stype: u32) -> Option<(&'static str, usize)> {
    table::STYPE_SIZES.binary_search_by_key(&stype, |r| r.0).ok().map(|i| (table::STYPE_SIZES[i].1, table::STYPE_SIZES[i].2))
}

/// Walks a create info's `pNext` chain and, if any link holds a callback,
/// returns a host copy of the chain up to the last such link with each
/// callback behind an entry; otherwise the guest's own pointer.
///
/// # Safety
///
/// `top` is null or a guest `const T*` whose chain is well formed.
pub(crate) unsafe fn translate_chain(rt: &Arc<Runtime>, cmd: &str, top: u64, keep: &mut Vec<Box<[u64]>>) -> Result<u64, Fault> {
    if top == 0 {
        return Ok(0);
    }
    // SAFETY: a link of the guest's chain, which starts with a
    // 4-byte sType and has pNext at offset 8 in every struct the gate saw.
    let stype = |p: u64| unsafe { (p as *const u32).read() };
    // SAFETY: as above.
    let next = |p: u64| unsafe { ((p + 8) as *const u64).read() };
    let mut nodes = Vec::new();
    let mut p = top;
    while p != 0 {
        if nodes.len() > 1024 {
            return Err(unsupported(cmd, "a pNext chain longer than 1024 links, or a cycle".into()));
        }
        nodes.push(p);
        p = next(p);
    }
    let pfn_of = |st: u32| table::PFN_MEMBERS.iter().filter(move |r| r.1 == Some(st));
    for &n in &nodes {
        if let Some((s, _)) = table::PFN_VIA_MEMBER.iter().find(|r| r.1 == Some(stype(n))) {
            return Err(unsupported(cmd, format!("{s} in the pNext chain hands the loader a function to call, \
                                                  and there is no guest driver to load")));
        }
    }
    let Some(last) = nodes.iter().rposition(|&n| pfn_of(stype(n)).next().is_some()) else {
        return Ok(top);
    };
    let mut copies: Vec<Box<[u64]>> = Vec::with_capacity(last + 1);
    for &n in &nodes[..=last] {
        let (_, size) = stype_size(stype(n)).ok_or_else(|| {
            unsupported(cmd, format!("sType {} in a chain that carries a callback has no known size", stype(n)))
        })?;
        let mut b = vec![0u64; size.div_ceil(8)].into_boxed_slice();
        // SAFETY: `size` bytes of the guest's struct, whose layout is the
        // host's (the gate), into a buffer at least as large.
        unsafe { std::ptr::copy_nonoverlapping(n as *const u8, b.as_mut_ptr().cast::<u8>(), size) };
        for &(_, _, off, _, pfn) in pfn_of(stype(n)) {
            b[off / 8] = callback_entry(rt, pfn, b[off / 8])?;
        }
        copies.push(b);
    }
    for i in 0..last {
        copies[i][1] = copies[i + 1].as_ptr() as u64;
    }
    let head = copies[0].as_ptr() as u64;
    keep.extend(copies);
    Ok(head)
}

/// Why this machine cannot run the layout gates, here and in `guest_xr`, if
/// it cannot: no `clang`, or one that cannot compile for a target the gates
/// compare. The Flatpak SDK's clang is x86 only, and `cargo test --workspace`
/// failed on it for want of a cross-compiler only these two tests use; they
/// skip and say so instead, as they do against other Vulkan headers. Asked
/// with an empty file, so a probe that does not compile still fails the gate.
#[cfg(test)]
pub(crate) fn layout_gate_cannot_compile() -> Option<String> {
    for target in ["aarch64-linux-android26", "x86_64-linux-gnu", "i686-linux-gnu"] {
        let out = match std::process::Command::new("clang")
            .args([&format!("--target={target}"), "-ffreestanding", "-nostdlibinc", "-x", "c", "-c", "/dev/null", "-o", "/dev/null"])
            .output()
        {
            Ok(out) => out,
            Err(e) => return Some(format!("cannot run clang: {e}")),
        };
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Some(format!("clang cannot compile for {target}: {}", err.lines().next().unwrap_or("").trim()));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// Defined data symbols of a little-endian ELF relocatable, 64- or
    /// 32-bit (the i686 control), by name.
    fn elf_symbols(path: &Path) -> HashMap<String, Vec<u8>> {
        let d = std::fs::read(path).unwrap();
        let u16_at = |o: usize| u16::from_le_bytes(d[o..o + 2].try_into().unwrap()) as usize;
        let u32_at = |o: usize| u32::from_le_bytes(d[o..o + 4].try_into().unwrap()) as usize;
        let u64_at = |o: usize| u64::from_le_bytes(d[o..o + 8].try_into().unwrap()) as usize;
        let wide = d[4] == 2;
        let word = |o: usize| if wide { u64_at(o) } else { u32_at(o) };
        let (shoff, shentsize, shnum) = if wide {
            (u64_at(0x28), u16_at(0x3a), u16_at(0x3c))
        } else {
            (u32_at(0x20), u16_at(0x2e), u16_at(0x30))
        };
        // (type, offset, size, link) of section i.
        let sec = |i: usize| {
            let b = shoff + i * shentsize;
            if wide {
                (u32_at(b + 4), u64_at(b + 24), u64_at(b + 32), u32_at(b + 40))
            } else {
                (u32_at(b + 4), u32_at(b + 16), u32_at(b + 20), u32_at(b + 24))
            }
        };
        let symsize = if wide { 24 } else { 16 };
        let mut out = HashMap::new();
        for i in 0..shnum {
            let (ty, off, size, link) = sec(i);
            if ty != 2 {
                continue;
            }
            let (_, stroff, _, _) = sec(link);
            for k in 0..size / symsize {
                let e = off + k * symsize;
                let (name_off, shndx, value, sz) = if wide {
                    (u32_at(e), u16_at(e + 6), word(e + 8), word(e + 16))
                } else {
                    (u32_at(e), u16_at(e + 14), word(e + 4), word(e + 8))
                };
                if shndx == 0 || shndx >= 0xff00 || sz == 0 {
                    continue;
                }
                let s = stroff + name_off;
                let end = s + d[s..].iter().position(|&b| b == 0).unwrap();
                let name = String::from_utf8_lossy(&d[s..end]).into_owned();
                let (sty, soff, _, _) = sec(shndx);
                let bytes = if sty == 8 { vec![0; sz] } else { d[soff + value..soff + value + sz].to_vec() };
                out.insert(name, bytes);
            }
        }
        out
    }

    /// The Vulkan headers the probe compiles against: `CORDIAL_VK_INCLUDE`,
    /// or the host's.
    fn include_dir() -> PathBuf {
        PathBuf::from(std::env::var("CORDIAL_VK_INCLUDE").unwrap_or_else(|_| "/usr/include".into()))
    }

    fn compile_probe(target: &str, dir: &Path) -> HashMap<String, Vec<u8>> {
        let inc = dir.join(format!("inc-{target}"));
        std::fs::create_dir_all(&inc).unwrap();
        for sub in ["vulkan", "vk_video"] {
            let link = inc.join(sub);
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(include_dir().join(sub), &link).unwrap();
        }
        let src = dir.join("probe.c");
        std::fs::write(&src, include_str!("guest_vk_probe.c")).unwrap();
        let obj = dir.join(format!("probe-{target}.o"));
        let out = Command::new("clang")
            .args([&format!("--target={target}"), "-ffreestanding", "-nostdlibinc", "-std=c11", "-w", "-c"])
            .arg("-I").arg(&inc).arg(&src).arg("-o").arg(&obj)
            .output()
            .expect("clang, which building Cordial already requires");
        assert!(out.status.success(), "the probe did not compile for {target} against {}: {}",
                include_dir().display(), String::from_utf8_lossy(&out.stderr));
        elf_symbols(&obj)
    }

    /// Labels for the probe's words and bitfield images, from its source.
    fn labels() -> (Vec<String>, Vec<String>) {
        let (mut words, mut bfs) = (Vec::new(), Vec::new());
        for line in include_str!("guest_vk_probe.c").lines().map(str::trim) {
            if let Some(r) = line.strip_prefix("S(") {
                let t = r.split(',').next().unwrap();
                words.extend([format!("sizeof({t})"), format!("alignof({t})"), format!("sType of {t}")]);
            } else if let Some(r) = line.strip_prefix("M(") {
                words.push(format!("offsetof({}", r.trim_end_matches(')').to_string() + ")"));
            } else if let Some(r) = line.strip_prefix("BF(") {
                let f: Vec<&str> = r.trim_end_matches(')').split(", ").collect();
                bfs.push(format!("{}.{}:{}", f[1], f[2], f[3]));
            }
        }
        words.push("terminator".into());
        (words, bfs)
    }

    fn words(o: &HashMap<String, Vec<u8>>) -> Vec<u64> {
        o["cordial_vk_probe"].chunks(8).map(|c| u64::from_le_bytes(c.try_into().unwrap())).collect()
    }

    fn differences(a: &HashMap<String, Vec<u8>>, b: &HashMap<String, Vec<u8>>) -> Vec<String> {
        let (lw, lb) = labels();
        let (wa, wb) = (words(a), words(b));
        assert_eq!(wa.len(), lw.len(), "probe words and labels disagree");
        let mut d: Vec<String> = lw.iter().zip(wa.iter().zip(&wb))
            .filter(|(_, (x, y))| x != y)
            .map(|(l, (x, y))| format!("{l}: {x} against {y}"))
            .collect();
        for (i, l) in lb.iter().enumerate() {
            let k = format!("cordial_vk_bf_{i}");
            if a[&k] != b[&k] {
                d.push(format!("bitfield {l}: {:02x?} against {:02x?}", a[&k], b[&k]));
            }
        }
        d
    }

    /// The gate design §3.2 asks for: every struct and union the guest could
    /// hand the host, as the Quest engine's compiler lays it out and as the
    /// host's does, with 0 differences. The control is a target that lays
    /// the same headers out differently (i686: 4-byte pointers and 4-byte
    /// aligned `uint64_t`), which must show differences, or the diff could be
    /// comparing nothing. It also checks the sizes and callback offsets the
    /// runtime was generated with against these headers.
    #[test]
    fn layout_gate() {
        // The probe asserts the exact header version the table was generated
        // from, so it cannot say anything against other headers. Arch's
        // rolling vulkan-headers (357 on 2026-10-02, against 341 here) made
        // this a packaging failure rather than a finding; skipping says so
        // instead. Point CORDIAL_VK_INCLUDE at matching headers to run it.
        let host = std::fs::read_to_string(include_dir().join("vulkan/vulkan_core.h"))
            .ok()
            .and_then(|h| {
                h.lines()
                    .find_map(|l| l.strip_prefix("#define VK_HEADER_VERSION "))
                    .and_then(|v| v.trim().parse::<u32>().ok())
            });
        if host != Some(table::HEADER_VERSION) {
            println!("layout gate skipped: headers at {} are VK_HEADER_VERSION {:?}, the table was generated from {}",
                     include_dir().display(), host, table::HEADER_VERSION);
            return;
        }
        if let Some(why) = super::layout_gate_cannot_compile() {
            println!("layout gate skipped: {why}");
            return;
        }
        let dir = std::env::temp_dir().join(format!("cordial-vk-gate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let arm = compile_probe("aarch64-linux-android26", &dir);
        let x86 = compile_probe("x86_64-linux-gnu", &dir);
        let i686 = compile_probe("i686-linux-gnu", &dir);
        let (lw, lb) = labels();
        let types = lw.iter().filter(|l| l.starts_with("sizeof(")).count();
        let d = differences(&arm, &x86);
        let control = differences(&arm, &i686);
        println!("layout gate: VK_HEADER_VERSION {}, vulkan_core.h, vulkan_android.h, vulkan_wayland.h at {}",
                 table::HEADER_VERSION, include_dir().display());
        println!("layout gate: {types} structs and unions, {} words, {} bitfield images", lw.len() - 1, lb.len());
        println!("layout gate: aarch64-linux-android26 against x86_64-linux-gnu: {} differences", d.len());
        for x in &d {
            println!("  {x}");
        }
        println!("layout gate: control, aarch64-linux-android26 against i686-linux-gnu: {} differences, e.g. {}",
                 control.len(), control.first().map_or("", String::as_str));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(d.is_empty(), "{} layout differences", d.len());
        assert!(control.len() > 100, "the control found only {} differences", control.len());

        // The generated sizes and callback offsets are these headers' too.
        let w = words(&x86);
        let mut size_of = HashMap::new();
        let mut stype_of = HashMap::new();
        let mut offset_of = HashMap::new();
        for (i, l) in lw.iter().enumerate() {
            if let Some(t) = l.strip_prefix("sizeof(").and_then(|r| r.strip_suffix(')')) {
                size_of.insert(t.to_owned(), w[i] as usize);
                stype_of.insert(t.to_owned(), w[i + 2]);
            } else if let Some(r) = l.strip_prefix("offsetof(").and_then(|r| r.strip_suffix(')')) {
                offset_of.insert(r.to_owned(), w[i] as usize);
            }
        }
        for &(st, name, size) in &table::STYPE_SIZES {
            assert_eq!(size_of.get(name), Some(&size), "{name}'s generated size");
            assert_eq!(stype_of.get(name), Some(&(st as u64)), "{name}'s generated sType");
        }
        // translate_chain reads sType at 0 and pNext at 8 of any link.
        for (t, &st) in &stype_of {
            if st != u64::MAX {
                assert_eq!(offset_of.get(&format!("{t}, sType")), Some(&0), "{t}.sType");
                assert_eq!(offset_of.get(&format!("{t}, pNext")), Some(&8), "{t}.pNext");
            }
        }
        for &(s, _, off, m, _) in &table::PFN_MEMBERS {
            assert_eq!(offset_of.get(&format!("{s}, {m}")), Some(&off), "{s}.{m}'s generated offset");
        }
    }

    #[test]
    fn the_table_is_whole() {
        let mut seen = std::collections::HashSet::new();
        for (n, _, _) in &table::VK {
            assert!(seen.insert(*n), "{n} twice");
        }
        for n in ["vkGetInstanceProcAddr", "vkGetDeviceProcAddr", "vkCreateInstance", "vkCreateAndroidSurfaceKHR",
                  "vkQueuePresentKHR", "vkCreateSwapchainKHR"] {
            assert!(signatures().contains_key(n), "{n} missing");
        }
        assert!(table::CHAIN_ARGS.iter().all(|r| !r.3), "a callback-carrying array argument appeared");
        // Every callback member has a signature to build its entry from.
        for &(_, _, _, _, pfn) in &table::PFN_MEMBERS {
            assert!(callback_sig(pfn).is_some(), "{pfn}");
        }
        assert!(table::STYPE_SIZES.windows(2).all(|w| w[0].0 < w[1].0), "sType table not sorted and unique");
    }

    /// arm64 code in a mapping of its own, for the translator to read.
    fn guest_code(words: &[u32]) -> cordial_guest::Mapping {
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        cordial_guest::Mapping::with_contents(&bytes, 0)
    }

    const RET: u32 = 0xd65f_03c0;

    /// The guest's allocation callbacks, called by the host as its own: each
    /// function becomes an entry into guest code, the copy is stable across
    /// calls, and the guest's struct is not written.
    #[test]
    fn allocator_callbacks_reach_guest_code() {
        let rt = Runtime::new(cordial_guest::Options::default());
        // pfnAllocation: return size + alignment. add x0, x1, x2; ret
        let alloc = guest_code(&[0x8b02_0020, RET]);
        // pfnFree: nothing. ret
        let free = guest_code(&[RET]);
        let g: [u64; 6] = [0x1234, alloc.addr(), 0, free.addr(), 0, 0];
        let h = allocator(&rt, g.as_ptr() as u64).unwrap();
        assert_ne!(h, g.as_ptr() as u64);
        assert_eq!(allocator(&rt, g.as_ptr() as u64).unwrap(), h, "one copy per allocator");
        // SAFETY: the copy this layer made.
        let hc = unsafe { (h as *const [u64; 6]).read() };
        assert_eq!(hc[0], 0x1234, "pUserData passes through");
        assert_eq!((hc[2], hc[4], hc[5]), (0, 0, 0), "absent optional functions stay absent");
        assert_ne!(hc[1], g[1], "pfnAllocation must not be the guest's arm64 address");
        for i in 0..1000u64 {
            // SAFETY: a host entry with PFN_vkAllocationFunction's signature.
            let f: extern "C" fn(*mut c_void, usize, usize, i32) -> *mut c_void =
                unsafe { std::mem::transmute(hc[1] as usize) };
            assert_eq!(f(std::ptr::null_mut(), 1000 + i as usize, 64, 1) as u64, 1064 + i);
            // SAFETY: as above, PFN_vkFreeFunction.
            let fr: extern "C" fn(*mut c_void, *mut c_void) = unsafe { std::mem::transmute(hc[3] as usize) };
            fr(std::ptr::null_mut(), std::ptr::null_mut());
        }
        assert_eq!(g, [0x1234, alloc.addr(), 0, free.addr(), 0, 0]);
    }

    /// A VkInstanceCreateInfo whose chain holds a validation-features link and
    /// then a debug-utils messenger: the copy reaches the same links, its
    /// callback is an entry that runs the guest's, and the tail beyond the
    /// last callback link is the guest's own.
    #[test]
    fn a_callback_deep_in_a_chain_is_translated() {
        let rt = Runtime::new(cordial_guest::Options::default());
        // PFN_vkDebugUtilsMessengerCallbackEXT: return messageTypes. mov w0, w1; ret
        let cb = guest_code(&[0x2a01_03e0, RET]);
        let tail = [1000061000u64 /* VK_STRUCTURE_TYPE_VALIDATION_FLAGS_EXT */, 0, 0, 0];
        let mut messenger = [0u64; 6];
        messenger[0] = 1000128004; // VK_STRUCTURE_TYPE_DEBUG_UTILS_MESSENGER_CREATE_INFO_EXT
        messenger[1] = tail.as_ptr() as u64;
        messenger[4] = cb.addr(); // pfnUserCallback, offset 32
        messenger[5] = 0x5555; // pUserData
        let mut features = [0u64; 6];
        features[0] = 1000247000; // VK_STRUCTURE_TYPE_VALIDATION_FEATURES_EXT, 48 bytes
        features[1] = messenger.as_ptr() as u64;
        let mut info = [0u64; 8];
        info[0] = 1; // VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO
        info[1] = features.as_ptr() as u64;
        info[2] = 7; // flags, to see it copied
        let before = (info, features, messenger);
        let mut keep = Vec::new();
        // SAFETY: a well-formed chain of ours.
        let h = unsafe { translate_chain(&rt, "vkCreateInstance", info.as_ptr() as u64, &mut keep) }.unwrap();
        assert_ne!(h, info.as_ptr() as u64);
        // SAFETY: the copies in `keep`.
        unsafe {
            let hi = h as *const u64;
            assert_eq!((hi.read(), hi.add(2).read()), (1, 7));
            let hf = hi.add(1).read() as *const u64;
            assert_ne!(hf as u64, features.as_ptr() as u64);
            assert_eq!(hf.read(), 1000247000);
            let hm = hf.add(1).read() as *const u64;
            assert_ne!(hm as u64, messenger.as_ptr() as u64);
            assert_eq!(hm.add(1).read(), tail.as_ptr() as u64, "the tail is the guest's own");
            assert_eq!(hm.add(5).read(), 0x5555);
            let f: extern "C" fn(i32, u32, *const c_void, *mut c_void) -> u32 =
                std::mem::transmute(hm.add(4).read() as usize);
            assert_eq!(f(0x10, 0x4, std::ptr::null(), std::ptr::null_mut()), 0x4);
        }
        assert_eq!((info, features, messenger), before, "the guest's chain was written");

        // Nothing to translate: the guest's pointer, untouched.
        let plain = [1u64, tail.as_ptr() as u64, 0, 0, 0, 0, 0, 0];
        // SAFETY: as above.
        let p = unsafe { translate_chain(&rt, "vkCreateInstance", plain.as_ptr() as u64, &mut keep) }.unwrap();
        assert_eq!(p, plain.as_ptr() as u64);

        // A driver-loading list is refused by name.
        let list = [1000459001u64, 0, 0, 0, 0];
        let mut info2 = [0u64; 8];
        info2[0] = 1;
        info2[1] = list.as_ptr() as u64;
        // SAFETY: as above.
        match unsafe { translate_chain(&rt, "vkCreateInstance", info2.as_ptr() as u64, &mut keep) } {
            Err(Fault::Unsupported { why, .. }) => assert!(why.contains("VkDirectDriverLoadingListLUNARG"), "{why}"),
            other => panic!("not refused: {other:?}"),
        }
    }

    /// The whole path on the real host, entered as the guest enters it: the
    /// guest `vkGetInstanceProcAddr` over Cordial's native one, then
    /// `vkCreateInstance` with a debug-utils messenger in its chain whose
    /// callback is arm64 code, the physical devices, and `vkCreateDevice` on
    /// the first discrete GPU -- every call through a generated stub. Needs a
    /// Vulkan ICD, so it is not in the default run:
    /// `cargo test -p cordial-runtime --lib guest_vk -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs a Vulkan driver on the host"]
    fn guest_vulkan_reaches_the_host_gpu() {
        let rt = Runtime::new(cordial_guest::Options::default());
        let native = crate::android::vulkan::get_instance_proc_addr_symbol().expect("host Vulkan") as usize;
        let gipa = rt.register("vkGetInstanceProcAddr", get_instance_proc_addr(native));
        let call = |pc: u64, a: &[u64]| cordial_guest::guest_call(&rt, pc, a, &[]).unwrap().x0;
        let proc = |inst: u64, n: &CStr| {
            let s = call(gipa, &[inst, n.as_ptr() as u64]);
            assert!(rt.stub_name(s).is_some(), "{n:?} came back as {s:#x}, not a guest stub");
            s
        };

        // The messenger's callback counts its calls through pUserData:
        // ldr x8, [x3]; add x8, x8, #1; str x8, [x3]; mov w0, wzr; ret
        let cb = guest_code(&[0xf940_0068, 0x9100_0508, 0xf900_0068, 0x2a1f_03e0, RET]);
        let mut calls = 0u64;
        let messenger: [u64; 6] = [1000128004, 0, 0x1111 << 32, 0x7, cb.addr(), &mut calls as *mut u64 as u64];
        let app: [u64; 6] = [0, 0, 0, 0, 0, ((1u64 << 22) | (1 << 12)) << 32];
        let exts = [c"VK_EXT_debug_utils".as_ptr() as u64];
        let info: [u64; 8] = [1, messenger.as_ptr() as u64, 0, app.as_ptr() as u64, 0, 0, 1, exts.as_ptr() as u64];
        let mut inst = 0u64;
        let r = call(proc(0, c"vkCreateInstance"), &[info.as_ptr() as u64, 0, &mut inst as *mut u64 as u64]);
        println!("vkCreateInstance -> {} (instance {inst:#x}), messenger callback ran {calls} times", r as i32);
        assert_eq!(r as i32, 0);

        let enumerate = proc(inst, c"vkEnumeratePhysicalDevices");
        let mut n = 0u32;
        assert_eq!(call(enumerate, &[inst, &mut n as *mut u32 as u64, 0]) as i32, 0);
        let mut devs = vec![0u64; n as usize];
        assert_eq!(call(enumerate, &[inst, &mut n as *mut u32 as u64, devs.as_mut_ptr() as u64]) as i32, 0);
        let props_fn = proc(inst, c"vkGetPhysicalDeviceProperties");
        let mut chosen = None;
        for &d in &devs {
            let mut p = vec![0u8; 1024];
            call(props_fn, &[d, p.as_mut_ptr() as u64]);
            let vendor = u32::from_le_bytes(p[8..12].try_into().unwrap());
            let ty = u32::from_le_bytes(p[16..20].try_into().unwrap());
            let name = CStr::from_bytes_until_nul(&p[20..276]).unwrap().to_string_lossy().into_owned();
            println!("physical device {d:#x}: vendor {vendor:#06x}, type {ty}, {name}");
            if ty == 2 && chosen.is_none() {
                chosen = Some((d, name));
            }
        }
        let (dev, name) = chosen.expect("a discrete GPU");

        let qfp = proc(inst, c"vkGetPhysicalDeviceQueueFamilyProperties");
        let mut nq = 0u32;
        call(qfp, &[dev, &mut nq as *mut u32 as u64, 0]);
        let mut fams = vec![[0u32; 6]; nq as usize];
        call(qfp, &[dev, &mut nq as *mut u32 as u64, fams.as_mut_ptr() as u64]);
        let family = fams.iter().position(|f| f[0] & 1 != 0).expect("a graphics queue") as u64;
        let priority = 1.0f32;
        let queue: [u64; 5] = [2, 0, family << 32, 1, &priority as *const f32 as u64];
        let dinfo: [u64; 9] = [3, 0, 1 << 32, queue.as_ptr() as u64, 0, 0, 0, 0, 0];
        let mut device = 0u64;
        let r = call(proc(inst, c"vkCreateDevice"), &[dev, dinfo.as_ptr() as u64, 0, &mut device as *mut u64 as u64]);
        println!("vkCreateDevice on {name} -> {} (device {device:#x})", r as i32);
        assert_eq!(r as i32, 0);
        let gdpa = proc(inst, c"vkGetDeviceProcAddr");
        let destroy = call(gdpa, &[device, c"vkDestroyDevice".as_ptr() as u64]);
        assert!(rt.stub_name(destroy).is_some());
        call(destroy, &[device, 0]);
        call(proc(inst, c"vkDestroyInstance"), &[inst, 0]);
        assert!(calls > 0, "the loader's messages never reached the guest callback");
    }
}

/// The image views and swapchains the engine holds, so a second destroy of
/// one is not handed to the driver.
///
/// Joining a game again after leaving one tears the VR render view down
/// (`pauseLuaAppAndDestroyIfNeeded destroySurfaceView:true` in its FLog), and
/// that teardown calls `vkDestroyImageView` twice on every view of the OpenXR
/// swapchain's images, from two call sites, with nothing in between. NVIDIA's
/// driver dereferences the freed view on the second call and faults, in every
/// run (docs/vr/play-button.md). Destroying a handle that is no longer valid
/// is undefined behaviour, so what a driver does with it is its own; that
/// Quest's driver survives it is INFERRED from the build shipping.
///
/// A destroy returns nothing, so not forwarding one the engine already made
/// changes nothing the engine can observe: the object is gone either way. It
/// is said in the log rather than hidden, and a handle this layer never saw
/// created is forwarded as before, since only a handle known to be dead is
/// known to be invalid. `CORDIAL_NO_VK_LIVE=1` is the control.
mod live {
    use super::*;

    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
    pub(super) enum Kind {
        ImageView,
        Swapchain,
    }

    pub(super) fn kind_of(name: &str) -> Option<(Kind, bool)> {
        if std::env::var_os("CORDIAL_NO_VK_LIVE").is_some_and(|v| !v.is_empty() && v != "0") {
            return None;
        }
        Some(match name {
            "vkCreateImageView" => (Kind::ImageView, true),
            "vkDestroyImageView" => (Kind::ImageView, false),
            "vkCreateSwapchainKHR" => (Kind::Swapchain, true),
            "vkDestroySwapchainKHR" => (Kind::Swapchain, false),
            _ => return None,
        })
    }

    #[derive(Default)]
    struct Book {
        live: std::collections::HashSet<(Kind, u64)>,
        /// Handles destroyed and not since made again, so a repeat can be
        /// told from a handle this layer never saw.
        dead: std::collections::HashSet<(Kind, u64)>,
        dropped: u64,
    }

    static BOOK: Mutex<Option<Book>> = Mutex::new(None);

    pub(super) fn tracked(name: &'static str, (kind, create): (Kind, bool), inner: Handler) -> Handler {
        if create {
            // vkCreate*(device, pCreateInfo, pAllocator, pHandle)
            return Box::new(move |c| {
                let out = c.x(3);
                inner(c)?;
                if c.x(0) as i32 == 0 && out != 0 {
                    // SAFETY: the guest's output handle, which the driver
                    // has just written on success; identity mapping.
                    let h = unsafe { (out as *const u64).read() };
                    let mut g = BOOK.lock().unwrap();
                    let b = g.get_or_insert_with(Book::default);
                    b.dead.remove(&(kind, h));
                    b.live.insert((kind, h));
                }
                Ok(())
            });
        }
        // vkDestroy*(device, handle, pAllocator)
        Box::new(move |c| {
            let h = c.x(1);
            if h != 0 {
                let mut g = BOOK.lock().unwrap();
                let b = g.get_or_insert_with(Book::default);
                if !b.live.remove(&(kind, h)) && b.dead.contains(&(kind, h)) {
                    b.dropped += 1;
                    let n = b.dropped;
                    drop(g);
                    if n <= 64 || n.is_power_of_two() {
                        eprintln!("[guest] vulkan: {name} of {h:#x}, already destroyed; not passed to the driver \
                                   ({n} so far)");
                    }
                    return Ok(());
                }
                // Bounded: a driver reuses freed addresses, and a create
                // takes its handle back out, so this stays small in practice.
                if b.dead.len() >= 1 << 16 {
                    b.dead.clear();
                }
                b.dead.insert((kind, h));
            }
            inner(c)
        })
    }
}
