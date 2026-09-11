use std::path::PathBuf;

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

    // AOSP bionic does not build with GCC; see docs/base-evaluation.md §2.1.
    let dst = cmake::Config::new(&native)
        .define("CMAKE_C_COMPILER", "clang")
        .define("CMAKE_CXX_COMPILER", "clang++")
        .define("CMAKE_BUILD_TYPE", "Release")
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
    // The C++ runtime: FreeBSD (and macOS) use libc++; glibc/Linux use libstdc++.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "freebsd" || target_os == "macos" || target_os == "openbsd" {
        println!("cargo:rustc-link-lib=dylib=c++");
    } else {
        println!("cargo:rustc-link-lib=dylib=stdc++");
    }
    println!("cargo:rustc-link-lib=dylib=z");
    // On FreeBSD dlopen/dlsym live in libc; there is a libdl compat stub but no
    // need to name it. Linux keeps them in a separate libdl.
    if target_os != "freebsd" {
        println!("cargo:rustc-link-lib=dylib=dl");
    }
    println!("cargo:rustc-link-lib=dylib=pthread");
    // FreeBSD has no epoll; libepoll-shim (a package) provides epoll_* over
    // kqueue. It lives under /usr/local, which is not on the default link path.
    if target_os == "freebsd" {
        println!("cargo:rustc-link-search=native=/usr/local/lib");
        println!("cargo:rustc-link-lib=dylib=epoll-shim");
    }

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
