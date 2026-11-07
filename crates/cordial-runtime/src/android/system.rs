//! Android's `/system` tree, built from what the host already has.
//!
//! Roblox asks for `/system/fonts/NotoSansCJK-Regular.ttc` during app startup.
//! On Android that file is always there. On a Linux host `/system` does not
//! exist, and the engine converts the failed lookup into an empty path and
//! throws `Path does not exist: ""` — an exception that names nothing, because
//! by the time it is raised there is no path left to name.
//!
//! Providing `/system` is the runtime's job, the same way `AAssetManager` and
//! `ALooper` are. The redirect itself lives at the libc boundary in
//! `native/system_paths.cpp`; this module builds the directory it points at.
//!
//! `/` is read-only on an image-based host, so a real `/system` is not an
//! option even with privileges — which is the other reason to do this in the
//! symbol table rather than on the filesystem.

use std::collections::BTreeMap;
use std::ffi::CString;
use std::path::{Path, PathBuf};

unsafe extern "C" {
    fn cordial_set_system_root(root: *const std::ffi::c_char);
    fn cordial_set_files_dir(dir: *const std::ffi::c_char);
}

/// Tell the framework layer which files directory this profile uses.
///
/// `native/android_classes.cpp`'s `files_dir()` computed
/// `cordial/instances/default/data` — the layout ADR-012 replaced — and followed
/// no `--profile`, so `Context.getFilesDir()` answered about a directory the
/// client was not using, identically for every profile. Call this with the same
/// path handed to `nativeSetFilesDirectory`, and before anything asks: the C++
/// side caches on first use.
pub fn set_files_dir(dir: &Path) {
    if let Ok(c) = CString::new(dir.to_string_lossy().as_bytes()) {
        // SAFETY: the callee copies the string; `c` need not outlive the call.
        unsafe { cordial_set_files_dir(c.as_ptr()) };
    }
}

/// Where the host's fonts live. Searched in order; later entries do not
/// override earlier ones, so a user font never shadows a system one by accident.
const FONT_DIRS: &[&str] = &[
    "/usr/share/fonts",
    "/usr/local/share/fonts",
    "/run/host/usr/share/fonts",
];

/// Android font file names worth answering for, and the host fonts that will
/// stand in when the exact name is missing.
///
/// Only the first of these is confirmed to be requested — the engine asked for
/// it by name, which is how this whole path was found. The rest are the ordinary
/// AOSP font set; answering them costs a symlink each and turns a future hard
/// failure into a slightly wrong glyph.
const ALIASES: &[(&str, &[&str])] = &[
    (
        "NotoSansCJK-Regular.ttc",
        &["NotoSansCJK-Regular.ttc", "NotoSansCJKjp-Regular.otf"],
    ),
    ("Roboto-Regular.ttf", &["NotoSans-Regular.ttf", "DejaVuSans.ttf"]),
    ("Roboto-Bold.ttf", &["NotoSans-Bold.ttf", "DejaVuSans-Bold.ttf"]),
    ("DroidSans.ttf", &["NotoSans-Regular.ttf", "DejaVuSans.ttf"]),
    ("DroidSansMono.ttf", &["NotoSansMono-Regular.ttf", "DejaVuSansMono.ttf"]),
    ("DroidSansFallback.ttf", &["NotoSansCJK-Regular.ttc", "DejaVuSans.ttf"]),
    ("NotoColorEmoji.ttf", &["NotoColorEmoji.ttf"]),
];

fn cache_root() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
        .join("cordial/system")
}

/// Every font file the host has, by file name. First writer wins.
fn host_fonts() -> BTreeMap<String, PathBuf> {
    let mut found = BTreeMap::new();
    let mut roots: Vec<PathBuf> = FONT_DIRS.iter().map(PathBuf::from).collect();
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(&home).join(".local/share/fonts"));
        roots.push(PathBuf::from(&home).join(".fonts"));
    }
    for root in roots {
        walk(&root, &mut found, 0);
    }
    found
}

fn walk(dir: &Path, out: &mut BTreeMap<String, PathBuf>, depth: usize) {
    // Font trees are shallow. The bound is against a symlink loop, not against
    // any real directory layout.
    if depth > 6 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out, depth + 1);
        } else if is_font(&p) {
            if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                out.entry(name.to_string()).or_insert(p.clone());
            }
        }
    }
}

fn is_font(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref(),
        Some("ttf" | "ttc" | "otf" | "otc")
    )
}

/// Pick the host file that should answer for an Android font name.
fn resolve<'a>(
    candidates: &[&str],
    fonts: &'a BTreeMap<String, PathBuf>,
) -> Option<&'a PathBuf> {
    candidates.iter().find_map(|c| fonts.get(*c))
}

/// Build the tree and arm the redirect. Returns the directory serving `/system`.
///
/// Never fails the launch: a client that starts with the wrong glyphs is more
/// useful than one that refuses to start because a font was missing. If nothing
/// can be linked the redirect is still armed, so the engine's lookups land in a
/// directory Cordial controls and show up in `CORDIAL_TRACE_PATHS=1` as a miss
/// there — which is a far better diagnostic than a miss in a `/system` that
/// cannot exist.
pub fn install() -> PathBuf {
    let root = cache_root();
    let fonts_dir = root.join("fonts");
    let _ = std::fs::create_dir_all(&fonts_dir);

    let fonts = host_fonts();
    let mut linked = 0usize;

    // Every host font under its own name first, so an exact request for a font
    // the host happens to have resolves to the real thing rather than a stand-in.
    for (name, path) in &fonts {
        if link(path, &fonts_dir.join(name)) {
            linked += 1;
        }
    }
    // Then the AOSP names the host does not have.
    for (android, candidates) in ALIASES {
        let dest = fonts_dir.join(android);
        if dest.exists() {
            continue;
        }
        if let Some(src) = resolve(candidates, &fonts) {
            if link(src, &dest) {
                linked += 1;
            }
        }
    }

    if std::env::var_os("CORDIAL_ANDROID_TRACE").is_some() {
        println!("[android] /system served from {} ({linked} fonts)", root.display());
    }

    if let Ok(c) = CString::new(root.to_string_lossy().as_bytes()) {
        // SAFETY: `cordial_set_system_root` copies the string; `c` need not
        // outlive the call.
        unsafe { cordial_set_system_root(c.as_ptr()) };
    }
    root
}

/// Symlink `src` to `dest`, replacing a stale link. Returns whether `dest` names
/// a usable font afterwards.
fn link(src: &Path, dest: &Path) -> bool {
    if let Ok(existing) = std::fs::read_link(dest) {
        if existing == src {
            return true;
        }
        let _ = std::fs::remove_file(dest);
    } else if dest.exists() {
        return true;
    }
    std::os::unix::fs::symlink(src, dest).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_font_files_are_indexed() {
        assert!(is_font(Path::new("/x/NotoSansCJK-Regular.ttc")));
        assert!(is_font(Path::new("/x/DejaVuSans.TTF")));
        assert!(!is_font(Path::new("/x/fonts.dir")));
        assert!(!is_font(Path::new("/x/README")));
    }

    #[test]
    fn an_alias_prefers_the_exact_name_over_a_stand_in() {
        let mut fonts = BTreeMap::new();
        fonts.insert("DejaVuSans.ttf".to_string(), PathBuf::from("/h/DejaVuSans.ttf"));
        fonts.insert(
            "NotoSans-Regular.ttf".to_string(),
            PathBuf::from("/h/NotoSans-Regular.ttf"),
        );
        // Roboto-Regular's candidate list puts NotoSans first, so it wins even
        // though DejaVu is also present.
        let got = resolve(&["NotoSans-Regular.ttf", "DejaVuSans.ttf"], &fonts);
        assert_eq!(got, Some(&PathBuf::from("/h/NotoSans-Regular.ttf")));
    }

    #[test]
    fn resolve_gives_up_rather_than_guessing() {
        let fonts = BTreeMap::new();
        assert!(resolve(&["NotoColorEmoji.ttf"], &fonts).is_none());
    }
}
