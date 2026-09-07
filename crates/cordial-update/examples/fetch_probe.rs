//! Fetch the newest Roblox build a mirror offers, and say what it is.
//!
//! **This exists for the `roblox_update.yml` workflow and nothing else.** When
//! a report arrives saying a build Cordial has never seen fails, the first
//! question is always the same pair: which build is current, and what does it
//! import that the stub table does not know about. Answering that used to mean
//! either driving the launcher's Download button by hand or waiting for the
//! reporter, and neither is a thing to do while triaging.
//!
//! It downloads into a directory you name and touches nothing else -- not the
//! engine cache, not a profile. The signature check is `provider::obtain`'s,
//! performed on whatever comes back from every source; this example does not
//! and must not do its own, for the reason `provider/mod.rs` gives at length.
//!
//! ```text
//! cargo run --release -p cordial-update --example fetch_probe -- /tmp/probe
//! ```
//!
//! Pair it with the symbol diff AGENTS.md prescribes:
//!
//! ```text
//! readelf --dyn-syms -W <dir>/lib/x86_64/libroblox.so \
//!   | awk '$7=="UND" {print $8}' | sed 's/@.*//' | sort -u > /tmp/new.txt
//! cut -f2 docs/analysis/undefined-symbols.tsv | sort -u > /tmp/old.txt
//! comm -23 /tmp/new.txt /tmp/old.txt
//! ```

use cordial_update::provider::{self, Cancel, Progress, Want};

fn main() {
    let into = match std::env::args().nth(1) {
        Some(d) => std::path::PathBuf::from(d),
        None => {
            eprintln!("usage: fetch_probe <directory to download into>");
            std::process::exit(2);
        }
    };
    if let Err(e) = std::fs::create_dir_all(&into) {
        eprintln!("cannot create {}: {e}", into.display());
        std::process::exit(1);
    }

    let cancel = Cancel::new();
    // Only the mirror. `local` would happily hand back the APK already on this
    // machine, which is the one build we know about and the opposite of the
    // question being asked.
    let mut last = String::new();
    let got = provider::obtain(Some("apkpure"), Want::Newest, &cancel, &into, &mut |p| {
        let line = match p {
            Progress::Asking { provider } => format!("asking {provider}"),
            Progress::Fetching { file, done, total } => match total {
                Some(t) => format!("{file}: {done}/{t} bytes"),
                None => format!("{file}: {done} bytes"),
            },
            other => format!("{other:?}"),
        };
        // One line per state rather than per chunk; a progress bar here would
        // be output nobody reads in a log.
        if line.split(':').next() != last.split(':').next() {
            println!("  {line}");
        }
        last = line;
    });

    match got {
        Ok(o) => {
            println!("\nversion   {} (code {})", o.version.name, o.version.code);
            println!("provider  {}", o.provider);
            println!("signed by {}", o.certificate_sha256);
            for path in o.archives.distinct() {
                let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                println!("archive   {} ({size} bytes)", path.display());
            }
        }
        Err(e) => {
            eprintln!("\nno build obtained: {e:?}");
            std::process::exit(1);
        }
    }
}
