use std::path::{Path, PathBuf};
use std::process::Command;

include!("../../patches/apply.rs");

fn main() {
    // dynarmic's A64 frontend has an x86-64 host backend and no arm64 one,
    // so on the aarch64 build this crate compiles to nothing. Checked against
    // the target, not the host: the aarch64 build is a cross build from an
    // x86-64 machine.
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("x86_64") {
        return;
    }
    // Without the feature the crate is empty (Cargo.toml says why), so
    // nothing here may need Boost, lld or llvm.
    if std::env::var_os("CARGO_FEATURE_DYNARMIC").is_none() {
        return;
    }

    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.join("../..").canonicalize().expect("workspace root");
    let dynarmic = root.join("third_party/dynarmic");
    if !dynarmic.join("src/dynarmic/interface/A64/a64.h").exists()
        || !dynarmic.join("externals/xbyak/xbyak/xbyak.h").exists()
    {
        panic!(
            "third_party/dynarmic or its externals are not checked out.\n\
             Run: git submodule update --init --recursive"
        );
    }

    // 0007 fixes a translator assertion the Quest build's constructors hit;
    // 0008 is the dispatch change measured in docs/vr/dynarmic-design.md §9.9.
    // patches/README.md has both, and patches/apply.rs why dynarmic is
    // compiled from a patched overlay under OUT_DIR rather than from the
    // submodule.
    let overlay = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("dynarmic");
    patched_overlay(
        &dynarmic,
        "",
        &overlay,
        &root.join("patches"),
        &["0007-dynarmic-keep-flag-setting-logic-ops", "0008-dynarmic-inline-dispatch-hit-paths"],
    );

    // Release regardless of the Cargo profile: a debug dynarmic is several
    // times slower at emitting code, which would make every timing taken
    // from a test build meaningless.
    let dst = cmake::Config::new(manifest.join("native"))
        .define("CMAKE_C_COMPILER", "clang")
        .define("CMAKE_CXX_COMPILER", "clang++")
        .define("CMAKE_BUILD_TYPE", "Release")
        .define("CORDIAL_DYNARMIC_DIR", &overlay)
        .build_target("cordial_guest_shim")
        .build();

    println!("cargo:rustc-link-search=native={}/build/lib", dst.display());
    println!("cargo:rustc-link-lib=static=cordial_guest_shim");
    println!("cargo:rustc-link-lib=static=dynarmic");
    println!("cargo:rustc-link-lib=static=mcl");
    println!("cargo:rustc-link-lib=static=fmt");
    println!("cargo:rustc-link-lib=static=Zydis");
    println!("cargo:rustc-link-lib=static=Zycore");
    println!("cargo:rustc-link-lib=dylib=stdc++");
    println!("cargo:rustc-link-lib=dylib=rt");

    // Watched recursively by directory, for the reason
    // cordial-linker-sys/build.rs gives: an unwatched source compiles
    // nothing and links the previous archive.
    println!("cargo:rerun-if-changed={}", manifest.join("native").display());
    println!("cargo:rerun-if-changed={}", dynarmic.join("src").display());

    build_test_guest(&manifest);
}

/// Compiles `tests/guest/*.c` into one flat arm64 image the tests map and run.
///
/// Compiled rather than hand-assembled so that the variadic calls, the
/// callback and the LL/SC loops the tests exercise are the ones a real AAPCS64
/// compiler emits -- the point of M1 is to find where the design's reading of
/// the ABI is wrong, and hand-written guest code would only encode the same
/// reading twice.
fn build_test_guest(manifest: &Path) {
    let src = manifest.join("tests/guest");
    println!("cargo:rerun-if-changed={}", src.display());
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let elf = out.join("guest.elf");
    let bin = out.join("guest.bin");

    let tool = |name: &str| -> Option<PathBuf> {
        // Ubuntu installs the LLVM tools unsuffixed only beside clang itself.
        let clang = which("clang")?;
        let beside = std::fs::canonicalize(&clang).ok()?.with_file_name(name);
        if beside.exists() { Some(beside) } else { which(name) }
    };
    // The image is only for this crate's tests, and a packaging build that
    // never runs them should not need an arm64-capable clang, lld and the
    // LLVM binutils for it. So anything missing fails the tests that include
    // the image, by name, at their compile, and leaves the library and every
    // other build alone. CI's test job has all of it. The Flatpak SDK's clang
    // is the case that found this: it is built for x86 only, and rejects
    // `--target=aarch64-linux-gnu` with "No available targets are compatible".
    let skip = |why: String| {
        println!("cargo:warning=cordial-guest's test image was not built ({why}); its tests will not compile");
        std::fs::write(&bin, b"").unwrap();
        std::fs::write(
            out.join("guest_syms.rs"),
            format!("compile_error!(\"cordial-guest's test image was not built: {why}\");\n"),
        )
        .unwrap();
    };
    let tools: Vec<_> = ["ld.lld", "llvm-objcopy", "llvm-nm"].iter().map(|n| (*n, tool(n))).collect();
    let missing: Vec<&str> = tools.iter().filter(|(_, p)| p.is_none()).map(|(n, _)| *n).collect();
    if !missing.is_empty() {
        return skip(format!("{} not found; install lld and llvm", missing.join(", ")));
    }
    let tool = |name: &str| -> PathBuf { tools.iter().find(|(n, _)| *n == name).unwrap().1.clone().unwrap() };

    let mut objs = Vec::new();
    for name in ["m1", "code"] {
        let obj = out.join(format!("{name}.o"));
        let compiled = Command::new("clang")
            .args(["--target=aarch64-linux-gnu", "-O2", "-ffreestanding", "-nostdlib",
                   "-fPIC", "-fvisibility=hidden", "-fno-stack-protector",
                   "-fno-asynchronous-unwind-tables", "-fno-unwind-tables",
                   // Inline LDXR/STXR loops rather than calls to the outline
                   // helpers, which would be unresolved in a flat image.
                   "-march=armv8-a", "-mno-outline-atomics", "-c"])
            .arg(src.join(format!("{name}.c")))
            .arg("-o").arg(&obj)
            .status()
            .is_ok_and(|s| s.success());
        if !compiled {
            return skip("clang cannot compile for aarch64-linux-gnu here".into());
        }
        objs.push(obj);
    }
    run(Command::new(tool("ld.lld"))
        .arg("-T").arg(src.join("flat.ld")).args(["-e", "guest_add"])
        .args(&objs).arg("-o").arg(&elf));
    run(Command::new(tool("llvm-objcopy")).args(["-O", "binary"]).arg(&elf).arg(&bin));

    // Entry points by name, from the linked image's own symbol table.
    let nm = Command::new(tool("llvm-nm")).arg("--defined-only").arg(&elf).output().unwrap();
    assert!(nm.status.success(), "llvm-nm failed");
    let mut rs = String::new();
    for line in String::from_utf8(nm.stdout).unwrap().lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() == 3 && (f[1] == "T" || f[1] == "t") && f[2].starts_with("guest_") {
            rs += &format!("pub const {}: u64 = 0x{};\n", f[2].to_uppercase(), f[0]);
        }
    }
    std::fs::write(out.join("guest_syms.rs"), rs).unwrap();
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|p| {
        std::env::split_paths(&p).map(|d| d.join(name)).find(|c| c.exists())
    })
}

fn run(cmd: &mut Command) {
    let status = cmd.status().unwrap_or_else(|e| panic!("{cmd:?}: {e}"));
    assert!(status.success(), "{cmd:?} failed");
}
