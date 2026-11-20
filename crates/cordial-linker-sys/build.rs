use std::path::PathBuf;

include!("../../patches/apply.rs");

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root");
    let native = root.join("native");

    if !root.join("third_party/mcpelauncher-linker/bionic/linker/linker.cpp").exists() {
        panic!(
            "third_party/mcpelauncher-linker is not checked out.\n\
             Run: git submodule update --init --recursive"
        );
    }

    // The loader patches that change behaviour rather than add a trace:
    // 0005 so a RELR-packed library is relocated at all, 0006 so the x86-64
    // client can link the Quest build's arm64 objects for the translator
    // (inert until `--guest-arm64` sets a guest machine). In that order, since
    // 0006 is made against 0005. patches/README.md has both, and
    // patches/apply.rs why the loader is compiled from a patched overlay
    // under OUT_DIR rather than from the submodule.
    let linker = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("mcpelauncher-linker");
    patched_overlay(
        &root.join("third_party/mcpelauncher-linker"),
        "bionic",
        &linker,
        &root.join("patches"),
        &["0005-parse-relr-dynamic-tags", "0006-link-guest-arch-objects"],
    );

    // Fedora 44's kernel headers use `__u128` in asm/sigcontext.h on aarch64,
    // and bionic's older `linux/types.h` shadows the host's and does not
    // define it, so linker.cpp stopped on "unknown type name '__u128'" and the
    // aarch64 .rpm failed. Ubuntu's older headers do not name it, so the type
    // is supplied only where the host's own header asks for it.
    let host_wants_u128 = cfg!(target_arch = "aarch64")
        && ["/usr/include/asm/sigcontext.h", "/usr/include/aarch64-linux-gnu/asm/sigcontext.h"]
            .iter()
            .any(|f| std::fs::read_to_string(f).map_or(false, |t| t.contains("__u128")));

    // AOSP bionic does not build with GCC; see docs/base-evaluation.md §2.1.
    let mut config = cmake::Config::new(&native);
    if host_wants_u128 {
        config.cflag("-D__u128=__uint128_t").cxxflag("-D__u128=__uint128_t");
    }
    let dst = config
        .define("CMAKE_C_COMPILER", "clang")
        .define("CMAKE_CXX_COMPILER", "clang++")
        .define("CMAKE_BUILD_TYPE", "Release")
        .define("CORDIAL_LINKER_DIR", &linker)
        // `CORDIAL_JNI_TRACE=1 cargo build` turns on libjnivm's trace.
        //
        // Not a convenience. libjnivm only emits `Constructed Unresolved
        // symbol` -- the one notice that the engine asked for a Java class or
        // method nobody wrote -- from inside `#ifdef JNI_TRACE`, so without
        // this the JNI section of `unimplemented::report` is empty for the
        // wrong reason and reads as "nothing was missing". It produced exactly
        // that false negative on its first real run.
        //
        // It is ruinously slow: the engine polls `MotionEvent.getRawX`/`getRawY`
        // per pointer per frame and this writes a line for each, unbuffered. Use
        // it to take an inventory, not to play.
        .define(
            "CORDIAL_JNI_TRACE",
            if std::env::var_os("CORDIAL_JNI_TRACE").is_some() { "ON" } else { "OFF" },
        )
        .build();

    println!("cargo:rustc-link-search=native={}/lib", dst.display());
    println!("cargo:rustc-link-lib=static=cordial_linker_shim");
    println!("cargo:rustc-link-lib=static=cordial_jni_shim");
    println!("cargo:rustc-link-lib=static=cordial_liblog");
    println!("cargo:rustc-link-lib=static=jnivm");
    // After jnivm: it is jnivm that references `Log::debug`, and a static
    // archive only satisfies symbols from archives listed after it.
    println!("cargo:rustc-link-lib=static=logger");
    println!("cargo:rustc-link-lib=static=linker");
    println!("cargo:rustc-link-lib=dylib=stdc++");
    println!("cargo:rustc-link-lib=dylib=z");
    println!("cargo:rustc-link-lib=dylib=dl");
    println!("cargo:rustc-link-lib=dylib=pthread");

    println!("cargo:rerun-if-env-changed=CORDIAL_JNI_TRACE");

    // Watch the whole native tree, not a hand-maintained list. A file missing
    // from that list is not a build error — Cargo simply does not re-run this
    // script, and the stale object from the previous build gets linked. That
    // failure looks exactly like code that compiled but had no effect.
    for entry in std::fs::read_dir(&native).expect("native/ is readable") {
        let path = entry.expect("readable dir entry").path();
        println!("cargo:rerun-if-changed={}", path.display());
    }

    // And the ported loader, for exactly the same reason.
    //
    // Watching only `native/` left a hole big enough to lose a change in: an
    // edit to `linker_phdr.cpp` compiles nothing, links the previous static
    // archive, and produces a binary that behaves as though the edit were never
    // made. That is worse than a build error, and it has already cost one
    // verification -- the change that stopped mapping the engine's text
    // writable appeared to have no effect until `cargo clean -p
    // cordial-linker-sys` forced the rebuild by hand.
    //
    // Cargo watches a directory recursively, so these are two lines rather than
    // a hand-maintained file list -- which is the same argument the comment
    // above makes about `native/`, and it should have been applied here at the
    // same time.
    for dir in ["third_party/mcpelauncher-linker/bionic/linker",
                "third_party/mcpelauncher-linker/bionic/libdl"] {
        let path = root.join(dir);
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}
