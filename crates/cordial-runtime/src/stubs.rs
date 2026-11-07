//! Stub implementations for every Android symbol Cordial does not provide yet.
//!
//! A stub records that it was called and returns zero. That makes the first
//! launch attempt produce a *prioritised* list of what to implement — which is
//! considerably more useful than a list of everything Roblox references.
//!
//! `CORDIAL_STUB_ABORT=1` aborts on the first hit instead, for bisecting a
//! specific failure.

use std::sync::Mutex;

include!(concat!(env!("OUT_DIR"), "/generated_stubs.rs"));

struct Hits {
    /// Call count per symbol index.
    counts: Vec<u32>,
    /// Symbol indices in first-hit order.
    order: Vec<usize>,
}

static HITS: Mutex<Option<Hits>> = Mutex::new(None);

fn abort_on_hit() -> bool {
    std::env::var_os("CORDIAL_STUB_ABORT").is_some()
}

fn quiet() -> bool {
    std::env::var_os("CORDIAL_STUB_QUIET").is_some()
}

/// Called by every generated stub.
pub fn hit(index: usize) -> i64 {
    let first = {
        let mut guard = HITS.lock().unwrap_or_else(|e| e.into_inner());
        let hits = guard.get_or_insert_with(|| Hits {
            counts: vec![0; SYMBOLS.len()],
            order: Vec::new(),
        });
        let first = hits.counts[index] == 0;
        hits.counts[index] += 1;
        if first {
            hits.order.push(index);
        }
        first
    };

    // Once per symbol into the central register, so the end-of-run report can
    // put libc stubs beside the JNI and framework gaps rather than in a table of
    // their own that has to be correlated by hand.
    //
    // Except where returning zero *is* the documented answer. That report exists
    // to name gaps, and a symbol whose contract says zero means "not enabled" is
    // not a gap -- listing it sends whoever reads the log to implement something
    // that is already correct, which is how ZSTD_trace_compress_begin came to be
    // reported as missing when it was answering properly.
    if first && !answering_correctly(SYMBOLS[index].0) {
        crate::unimplemented::record(crate::unimplemented::Kind::LibcStub, SYMBOLS[index].0);
    }
    if first && !quiet() {
        eprintln!("[stub] {}", SYMBOLS[index].0);
    }
    if abort_on_hit() {
        eprintln!("[stub] CORDIAL_STUB_ABORT set — aborting on {}", SYMBOLS[index].0);
        report();
        std::process::abort();
    }
    if let Some(why) = is_fatal(SYMBOLS[index].0) {
        fatal(SYMBOLS[index].0, why);
    }
    0
}

/// Symbols where returning `0` is not a harmless placeholder but a lie the
/// caller cannot survive, each with the sentence a person hitting it should
/// read.
///
/// Every stub returns `0`, which is right for most of them: a counter nobody
/// reads, a capability query answered "no". For an entry here it is fatal, and
/// in the worst way — the caller is *told it succeeded* and proceeds.
///
/// **The list is empty, and that does not mean the danger is gone.** It held
/// `pthread_once`, `pthread_getspecific`, `pthread_setspecific`,
/// `pthread_key_create` and `pthread_key_delete` until those five were
/// implemented in `bionic::pthread`; they are resolved now, never reach a stub,
/// and an entry for a symbol that cannot be hit is a comment that lies.
///
/// Nothing replaced them, deliberately. `--lib-dir` without `--host-libc` still
/// ends in SIGSEGV, now further into the engine's static initialisers, with
/// `__cxa_atexit` the last stub reported before the core dump — but *which*
/// stub is the one it cannot survive has not been established, and the obvious
/// guess is wrong: `memset` is stubbed in that configuration and the same run
/// carried on through five more first-hit stubs after calling it. Do not add a
/// symbol here on the strength of it looking dangerous. Add it when a run shows
/// the process dying on it.
///
/// The underlying problem is larger than this list. Bare `--lib-dir` stubs 358
/// libc symbols, `memset` and `pthread_mutex_lock` among them; it is a
/// diagnostic configuration that produces a prioritised work queue, not one the
/// engine can run in. What closes it is a real bionic shim, not more entries
/// here.
const FATAL: &[(&str, &str)] = &[];

fn is_fatal(symbol: &str) -> Option<&'static str> {
    FATAL
        .iter()
        .find(|(name, _)| *name == symbol)
        .map(|(_, why)| *why)
}

/// Say what happened and stop, rather than returning a lie and crashing later.
///
/// A named exit beats a SIGSEGV by the whole distance between "this symbol is
/// not implemented, here is the switch that resolves it" and a core dump three
/// frames into someone else's thread-local teardown.
fn fatal(symbol: &str, why: &str) -> ! {
    eprintln!();
    eprintln!("[stub] {symbol} is not implemented, and returning a placeholder would crash.");
    eprintln!("       {why}");
    eprintln!("       Pass --host-libc to resolve libc from the host, which is what the");
    eprintln!("       --game-activity path does and why that path does not hit this.");
    report();
    // `_exit` for the same reason the loader uses it: Roblox's static
    // initialisers registered atexit handlers that expect a live Android
    // process, and running them here would replace this message with the crash
    // it exists to prevent.
    unsafe { libc_exit(1) }
}

unsafe extern "C" {
    #[link_name = "_exit"]
    fn libc_exit(status: std::ffi::c_int) -> !;
}

/// Every stub called at least once, most-called first. This is the work queue.
/// Symbols where a zero return is the contract rather than a shortfall.
///
/// zstd's tracing hooks are the whole list today. `zstd_trace.h` says of
/// `ZSTD_trace_compress_begin` and `ZSTD_trace_decompress_begin`: "@returns
/// Non-zero if tracing is enabled". Zero therefore means tracing is off, and
/// zstd skips the matching `_end` call entirely. Implementing them to return
/// non-zero would oblige us to produce `ZSTD_Trace` records that nothing here
/// consumes -- the stub would start lying, which is the failure AGENTS.md's rule
/// about stubs is written to prevent, arriving from the unusual direction.
///
/// They stay in the stub table so the symbol still resolves and the call still
/// gets an answer; what changes is that the end-of-run report stops calling that
/// answer a gap.
fn answering_correctly(symbol: &str) -> bool {
    matches!(
        symbol,
        "ZSTD_trace_compress_begin"
            | "ZSTD_trace_decompress_begin"
            | "ZSTD_trace_compress_end"
            | "ZSTD_trace_decompress_end"
    )
}

pub fn report() {
    let guard = HITS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(hits) = guard.as_ref() else {
        eprintln!("\n=== no stubs were called ===");
        return;
    };

    let mut called: Vec<(usize, u32)> = hits
        .order
        .iter()
        .map(|&i| (i, hits.counts[i]))
        .collect();
    called.sort_by(|a, b| b.1.cmp(&a.1));

    eprintln!(
        "\n=== stubs called: {} distinct of {} ===",
        called.len(),
        SYMBOLS.len()
    );
    for (i, count) in called {
        eprintln!("  {count:>9}  {}", SYMBOLS[i].0);
    }
}
