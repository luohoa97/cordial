//! `AAssetManager` — Roblox's assets, read out of the APK.
//!
//! This is the gate on rendering: the engine loads every shader, font and
//! texture through it, so nothing appears on screen until it works.
//!
//! Roblox uses the whole-buffer half of the API (`AAsset_getBuffer` and
//! `AAsset_getLength`) rather than streaming `AAsset_read`, which makes the
//! implementation much simpler than the general case — an asset is just its
//! decompressed bytes, held until closed.
//!
//! Android serves assets from `assets/` inside the APK. That is a plain zip, and
//! the paths Roblox passes are relative to it.
//!
//! Before the zip is ever consulted, every lookup is checked against the
//! overlay stack — see the `overlay` section below. That is
//! [ADR-010](../../../../docs/adr/ADR-010-plugin-asset-overlays.md): plugins
//! and the user may provide files that resolve in place of the APK's own,
//! without Cordial ever writing into the APK or into anything extracted from
//! it.

use crate::ffi_util::{borrow_request, drop_handle, handle_mut, handle_ref, into_handle, store_out, write_response};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::{c_char, c_int, c_void};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// One opened asset. The pointer handed back to Roblox is a `Box::into_raw` of
/// this, which is what an `AAsset*` is as far as the engine is concerned.
struct Asset {
    /// Borrowed from the cache, not copied. Roblox opens the same assets
    /// repeatedly; copying each time would double the memory for no benefit,
    /// since the cached bytes already live for the process.
    bytes: &'static [u8],
}

/// A reader over the APK that can be cloned cheaply and used from any thread
/// at the same time as its clones.
///
/// **This exists to stop the zip's central directory being parsed once per
/// asset**, which is what `Manager::read` used to do — `File::open` plus
/// `ZipArchive::new` on every uncached name, and `ZipArchive::new` walks all
/// 1,835 central-directory entries before it can answer anything. A menu that
/// opens a thousand small UI textures paid for a thousand full re-indexes of
/// the archive, plus a thousand `open(2)`s.
///
/// The shape of the fix comes from the crate's own type:
/// `ZipArchive<R> { reader: R, shared: Arc<Shared> }`, deriving `Clone`. The
/// parsed directory is *already* behind an `Arc`, so cloning an archive costs
/// a refcount bump and copies nothing — provided `R` is `Clone`, which
/// `std::fs::File` is not. This is the `R` that is.
///
/// **Positioned reads, not a shared seek offset.** Every read goes through
/// `pread(2)` via [`FileExt::read_at`] with this handle's own `pos`, so two
/// clones reading different entries at the same time cannot move each other's
/// cursor. Sharing one `File` and calling `Seek` on it would be a data race
/// that shows up as an asset served the bytes of a different asset — the worst
/// possible failure mode here, because the engine would render it rather than
/// report it.
///
/// One file descriptor for the process, rather than one per read. The
/// alternative considered was a pool of `ZipArchive<File>`: it also parses the
/// directory once per pooled archive, but it needs a lock to hand one out, a
/// cap to bound memory, and a policy for what happens when the pool is empty.
/// This needs none of the three, because there is nothing to hand out.
#[derive(Clone)]
struct ApkReader {
    file: Arc<File>,
    pos: u64,
    len: u64,
}

impl ApkReader {
    fn open(path: &Path) -> std::io::Result<ApkReader> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Ok(ApkReader { file: Arc::new(file), pos: 0, len })
    }
}

impl Read for ApkReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.file.read_at(buf, self.pos)?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for ApkReader {
    fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
        // Saturating rather than wrapping, and an explicit error for a
        // negative result: the zip reader seeks backwards from the end to find
        // the end-of-central-directory record, so `End(-22)` and friends are
        // the ordinary case here, not an edge one.
        let next = match from {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::End(n) => self.len as i64 + n,
            SeekFrom::Current(n) => self.pos as i64 + n,
        };
        if next < 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "seek before the start of the APK",
            ));
        }
        self.pos = next as u64;
        Ok(self.pos)
    }
}

struct Manager {
    apk: PathBuf,
    /// The archive with its central directory already parsed, cloned per read.
    ///
    /// Built once, lazily, so that a `set_apk` on a file that later becomes
    /// unreadable still fails at the read rather than at startup — the
    /// behaviour before this existed. `None` means the archive could not be
    /// opened or parsed at all, which is reported by the caller as a miss, as
    /// it always was.
    ///
    /// **Deliberately not a `Mutex<ZipArchive<_>>`.** `by_name` needs `&mut`,
    /// so one shared archive would mean holding a lock across the read and the
    /// inflate — serialising every asset the engine asks for, on the twenty-odd
    /// threads it asks from. The previous code was slow but at least concurrent
    /// (the cache lock is released before the zip work, which is why it never
    /// showed up as contention), and a fix that made the fast path serial would
    /// have traded one bug for a worse one. A `OnceLock` of a *template* that
    /// each read clones keeps the concurrency and removes the re-parse.
    archive: OnceLock<Option<zip::ZipArchive<ApkReader>>>,
    /// Decompressed contents, keyed by path inside `assets/`.
    ///
    /// Roblox reopens the same assets repeatedly during startup, and zip
    /// decompression is not free. Caching also means `AAsset_getBuffer` can hand
    /// out a pointer that stays valid after the asset is closed, which the API
    /// does not promise but callers routinely assume.
    cache: Mutex<HashMap<String, &'static [u8]>>,
}

impl Manager {
    /// A private archive handle sharing the one parsed central directory, or
    /// `None` if the APK could not be read.
    fn archive(&self) -> Option<zip::ZipArchive<ApkReader>> {
        self.archive
            .get_or_init(|| {
                let reader = ApkReader::open(&self.apk).ok()?;
                let zip = zip::ZipArchive::new(reader).ok()?;
                if trace_assets_enabled() {
                    eprintln!(
                        "[asset] indexed {} entries in {}",
                        zip.len(),
                        self.apk.display()
                    );
                }
                Some(zip)
            })
            .clone()
    }
}

static MANAGER: OnceLock<Manager> = OnceLock::new();

/// Point the asset manager at an APK. Must be called before Roblox asks for
/// anything; `AAssetManager_open` fails cleanly until it is.
pub fn set_apk(path: &Path) -> Result<(), String> {
    // Fail now, with a path in the message, rather than at the first asset.
    File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    MANAGER
        .set(Manager {
            apk: path.to_path_buf(),
            archive: OnceLock::new(),
            cache: Mutex::new(HashMap::new()),
        })
        .map_err(|_| "an APK is already set".to_string())
}

pub fn is_configured() -> bool {
    MANAGER.get().is_some()
}

/// Read `assets/<name>` the same way the engine's own `AAssetManager_open`
/// does, overlays and all.
///
/// Exposed for the parts of Cordial that need something out of the APK for
/// themselves rather than on the engine's behalf -- the editor font is the
/// first. Going through the same path means an asset overlay can replace it,
/// which is the consistent answer even though nobody is likely to re-skin a
/// font.
pub fn read_asset(name: &str) -> Option<&'static [u8]> {
    MANAGER.get()?.read(name)
}

/// `CORDIAL_TRACE_ASSETS=1` is the one env var this module's tracing is
/// documented to key off; `CORDIAL_TRACE` is accepted too where it was
/// already wired in, but never required — `CORDIAL_TRACE=1` wraps variadic
/// functions with fixed-arity declarations and aborts the engine
/// (AGENTS.md), so a trace line gated on it alone is not reachable without
/// killing the client. That was true of the miss line below until this
/// change: it checked `CORDIAL_TRACE` only, so a miss — the empty-string
/// signature this investigation is chasing — could never actually be
/// observed. See docs/analysis/flag-init.md §34, "What is left after
/// twenty-four".
fn trace_assets_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os("CORDIAL_TRACE_ASSETS").is_some()
            || std::env::var_os("CORDIAL_TRACE").is_some()
    })
}

impl Manager {
    /// Read `assets/<name>`, consulting the overlay stack before the APK, and
    /// caching whichever bytes win. Every outcome — cache, overlay, APK, or
    /// miss — is traced under `CORDIAL_TRACE_ASSETS=1` so a miss is visible,
    /// not just a hit.
    fn read(&self, name: &str) -> Option<&'static [u8]> {
        if let Some(bytes) = self.cache.lock().ok()?.get(name) {
            if trace_assets_enabled() {
                eprintln!("[asset] hit (cache): {name}");
            }
            // Not re-recorded: the first request for this name already
            // recorded which layer answered it, and overwriting that with
            // "cache" would erase the one fact the recorder exists to keep.
            return Some(bytes);
        }

        if let Some((source, path)) = resolve_overlay(name) {
            // A file that resolved a moment ago and is gone now (removed
            // mid-registration-change, races with `unregister_plugin_root`) is
            // not a reason to fail a lookup that has a perfectly good
            // fallback — fall through to the APK exactly as if the overlay
            // had never had it.
            if let Ok(bytes) = std::fs::read(&path) {
                // This line was the only way to observe that an overlay had
                // applied, and it was gated on a flag AGENTS.md tells people
                // never to set: `CORDIAL_TRACE=1` wraps variadic functions with
                // fixed-arity declarations and aborts the engine. So the one
                // diagnostic for ADR-010 could not be reached without killing
                // the client, which means nobody could confirm an overlay was
                // working except by looking at the screen.
                if trace_assets_enabled() {
                    eprintln!("[asset] hit (overlay): {name} from {}", source.describe());
                }
                record(name, Served::Overlay(source));
                let leaked: &'static [u8] = Vec::leak(bytes);
                self.cache.lock().ok()?.insert(name.to_string(), leaked);
                return Some(leaked);
            }
        }

        let Some(bytes) = (|| -> Option<Vec<u8>> {
            // `archive()` parses the central directory once for the process
            // and hands back a clone that shares it; this used to open the
            // APK and re-index all 1,835 entries here, per asset.
            let mut zip = self.archive()?;
            let mut entry = zip.by_name(&format!("assets/{name}")).ok()?;
            let mut bytes = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut bytes).ok()?;
            Some(bytes)
        })() else {
            if trace_assets_enabled() {
                eprintln!("[asset] miss: {name}");
            }
            record(name, Served::Missing);
            return None;
        };

        if trace_assets_enabled() {
            eprintln!("[asset] hit (apk): {name}");
        }
        record(name, Served::Apk);

        // Deliberately leaked. Asset lifetimes in this API are the caller's to
        // manage and Roblox is not careful about them; an asset outliving its
        // AAsset handle is far cheaper than a dangling buffer, and the total is
        // bounded by what the APK contains.
        let leaked: &'static [u8] = Vec::leak(bytes);
        self.cache.lock().ok()?.insert(name.to_string(), leaked);
        Some(leaked)
    }
}

// ---------------------------------------------------------------- the overlay
//
// ADR-010 decided that a plugin, or the user directly, may provide a file that
// resolves in place of the APK's own for the same name.
// [ADR-021](../../../../docs/adr/ADR-021-everything-is-a-plugin.md) settles how,
// and extends it to the second route the engine uses. The whole mechanism is a
// lookup interposed ahead of the zip read above — nothing is ever copied into
// the APK, nothing is ever copied into `extract_to`'s output, and removing a
// root is exactly as simple as no longer consulting it. There is no cleanup
// step because there was never a write to undo.
//
// **Why interception rather than a mount.** The assets are zip entries inside
// `base.apk`, so overlayfs has nothing to overlay without extracting the whole
// archive first, and Flatpak cannot mount overlayfs unprivileged in any case.
// Interception also yields the request trace, the two orphan signals and the
// shadow report below, none of which a mount can provide.
//
// **Why an open-intercept is not defeated by mmap.** The hazard in general is
// a client that maps the archive once and reads each asset at an offset inside
// that mapping, so no asset ever gets an fd. Sampled every 400 ms across two
// complete cold launches — 136 samples, `libroblox.so` mapped in every one —
// `/proc/<pid>/maps` and `/proc/<pid>/fd` held zero references to `base.apk`
// and zero to the extracted asset tree. There was never a whole-archive
// mapping to find. ADR-021 records the reading.
//
// One thing this does *not* attempt: if an asset is already cached (served
// once, from either the overlay or the APK), it stays cached for the rest of
// the process — the same "cached forever" contract `Manager::read` already
// gives the APK path, because Roblox is handed a pointer that has to remain
// valid. Reloading the overlay after an asset has already been served does not
// retroactively change what a held pointer points at, and `reload` reports how
// many names it could not affect for exactly that reason.

/// Where an overlaid file came from. Mirrors [`crate::flags::Source`] — naming
/// the origin is what makes "why did this asset change" answerable after the
/// fact, and what lets removing one root put back exactly what it replaced and
/// nothing else.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OverlaySource {
    /// A plugin's overlay root, named by the plugin's id.
    ///
    /// Ordered before `User` deliberately: the derived `Ord` is what sorts a
    /// shadow report, and the user's layer belongs at the top of it because
    /// the user's layer is the one that won.
    Plugin(String),
    /// The user's own overlay root — see [`user_root`].
    User,
}

impl OverlaySource {
    pub fn describe(&self) -> String {
        match self {
            OverlaySource::User => "user".into(),
            OverlaySource::Plugin(id) => format!("plugin:{id}"),
        }
    }
}

/// One overlay root and who owns it: a directory that mirrors the APK's
/// `assets/` layout.
#[derive(Debug, Clone)]
struct OverlayLayer {
    source: OverlaySource,
    root: PathBuf,
}

/// Every plugin-registered overlay root, in registration order. The user's
/// root is not kept here — it has nowhere to be "registered" from, it is
/// always whatever `user_root` resolves to — so this only ever holds plugins.
static PLUGIN_OVERLAYS: Mutex<Vec<OverlayLayer>> = Mutex::new(Vec::new());

/// What one name resolves to, and what it beat to get there.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub source: OverlaySource,
    pub path: PathBuf,
    /// Every layer that also offered this name and lost, lowest priority
    /// first. Empty in the ordinary case.
    ///
    /// Recorded at index-build time because it is free there and impossible
    /// afterwards. "Why did this file not change" is otherwise
    /// indistinguishable from "the overlay is broken", and it is the question
    /// users ask most.
    pub shadowed: Vec<OverlaySource>,
}

/// Every name any layer offers, merged in precedence order, built once.
///
/// **Why an index and not a walk per lookup.** Resolution used to canonicalise
/// each root and stat the candidate on every single asset open, which is a
/// stat storm on the hottest path in asset loading and gets worse with each
/// overlay installed. Walking each root once instead turns a lookup into one
/// hash probe, and a name absent from the map is absent from every overlay —
/// so the negative answer costs the same probe and there is no separate
/// negative cache to keep coherent with this one.
#[derive(Debug, Default)]
pub struct Index {
    files: HashMap<String, Resolved>,
    /// The roots this index was built from, so a report can say which layers
    /// were consulted even when they contributed nothing.
    layers: Vec<(OverlaySource, PathBuf)>,
}

impl Index {
    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&Resolved> {
        self.files.get(name)
    }

    /// Every name the overlay stack provides, in sorted order.
    pub fn names(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.files.keys().map(String::as_str).collect();
        v.sort_unstable();
        v
    }

    /// Every name more than one layer offered, with the winner and the losers.
    /// Sorted, so two runs over the same roots report the same thing.
    pub fn shadowing(&self) -> Vec<(&str, &Resolved)> {
        let mut v: Vec<(&str, &Resolved)> =
            self.files.iter().filter(|(_, r)| !r.shadowed.is_empty()).map(|(n, r)| (n.as_str(), r)).collect();
        v.sort_unstable_by_key(|(n, _)| *n);
        v
    }
}

/// The built index, or `None` when the stack has changed and it needs
/// rebuilding. Held behind an `Arc` so a lookup clones a pointer and never
/// holds the lock across a filesystem read.
static INDEX: Mutex<Option<Arc<Index>>> = Mutex::new(None);

/// The user's own overlay root: `$XDG_CONFIG_HOME/cordial/overlay`, falling
/// back to `$HOME/.config/cordial/overlay`, overridable with `CORDIAL_OVERLAY`.
/// Mirrors the APK's `assets/` layout exactly, the same shape Sober's
/// `asset_overlay` uses.
///
/// Not required to exist. A missing directory contributes no names — see
/// `walk_root`, which fails closed rather than needing a separate existence
/// check here.
pub fn user_root() -> PathBuf {
    std::env::var_os("CORDIAL_OVERLAY")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
                .unwrap_or_else(std::env::temp_dir)
                .join("cordial/overlay")
        })
}

/// Register (or re-register) a plugin's overlay root.
///
/// Re-registering an id already on the stack moves it to the end rather than
/// leaving a stale entry in its old position, so "last registered wins" also
/// covers a plugin that changes its own root mid-session rather than only
/// covering the order plugins first started in.
pub fn register_plugin_root(id: &str, root: PathBuf) {
    let source = OverlaySource::Plugin(id.to_string());
    if let Ok(mut stack) = PLUGIN_OVERLAYS.lock() {
        stack.retain(|l| l.source != source);
        stack.push(OverlayLayer { source, root });
    }
    invalidate();
}

/// Remove a plugin's overlay root. Everything it was serving falls straight
/// back to whatever would have resolved without it — a lower-priority overlay,
/// or the APK — because nothing was ever written into either to begin with.
pub fn unregister_plugin_root(id: &str) {
    let source = OverlaySource::Plugin(id.to_string());
    if let Ok(mut stack) = PLUGIN_OVERLAYS.lock() {
        stack.retain(|l| l.source != source);
    }
    invalidate();
}

/// Drop the built index so the next lookup rebuilds it.
fn invalidate() {
    if let Ok(mut held) = INDEX.lock() {
        *held = None;
    }
}

/// The layer stack in precedence order, **lowest first**: plugins in
/// registration order, then the user, who therefore beats every plugin.
///
/// This mirrors the rule [`crate::flags::resolve`] already uses, for the
/// reason recorded there — an explicit choice the user made must not be
/// silently overridden by something they installed to do something else.
fn layers() -> Vec<OverlayLayer> {
    let mut layers = PLUGIN_OVERLAYS.lock().map(|s| s.clone()).unwrap_or_default();
    layers.push(OverlayLayer { source: OverlaySource::User, root: user_root() });
    layers
}

/// Every file under `root`, as names relative to it with `/` separators.
///
/// A symlink is followed only when it stays inside the root — the containment
/// check that used to run per lookup, moved here so it runs once per file at
/// build time instead. An entry that escapes is dropped from the index
/// entirely, so no later code has to remember to re-check it, which is what
/// let the per-lookup version go. A root that does not exist, or cannot be
/// canonicalised, contributes nothing — which is what makes the common case of
/// "no user overlay directory" free rather than an error.
fn walk_root(root: &Path) -> Vec<(String, PathBuf)> {
    let Ok(root) = root.canonicalize() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            // Canonicalising every entry is what keeps a symlink from
            // smuggling a file in from outside the root. Doing it here rather
            // than at lookup time means the cost is paid once per file at
            // build, not once per asset open — and an entry that escapes is
            // dropped from the index entirely, so no later code has to
            // remember to re-check it.
            let Ok(real) = path.canonicalize() else {
                continue;
            };
            if !real.starts_with(&root) {
                continue;
            }
            if real.is_dir() {
                stack.push(real);
                continue;
            }
            let Ok(rel) = real.strip_prefix(&root) else {
                continue;
            };
            let name = rel.components().filter_map(|c| c.as_os_str().to_str()).collect::<Vec<_>>().join("/");
            if !name.is_empty() {
                out.push((name, real));
            }
        }
    }
    out
}

/// Merge every layer into one map, later layers winning and earlier ones
/// recorded as shadowed.
fn build_index(layers: &[OverlayLayer]) -> Index {
    let mut files: HashMap<String, Resolved> = HashMap::new();
    for layer in layers {
        for (name, path) in walk_root(&layer.root) {
            match files.get_mut(&name) {
                Some(existing) => {
                    // The incumbent loses to a later layer, and is remembered
                    // rather than dropped: the losers are the whole content of
                    // the shadow report, and they are impossible to recover
                    // once the map holds only winners.
                    let beaten = std::mem::replace(&mut existing.source, layer.source.clone());
                    existing.shadowed.push(beaten);
                    existing.path = path;
                }
                None => {
                    files.insert(name, Resolved { source: layer.source.clone(), path, shadowed: Vec::new() });
                }
            }
        }
    }
    Index { files, layers: layers.iter().map(|l| (l.source.clone(), l.root.clone())).collect() }
}

/// The current index, building it if the stack has changed since the last one.
pub fn index() -> Arc<Index> {
    // Built outside the lock is tempting and wrong here: two threads would
    // each walk every root. Roots are walked at startup, before Roblox asks
    // for anything, so holding the lock across the walk costs nothing that
    // anybody waits on.
    let mut held = INDEX.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(index) = held.as_ref() {
        return Arc::clone(index);
    }
    let built = Arc::new(build_index(&layers()));
    *held = Some(Arc::clone(&built));
    built
}

/// What a reload actually achieved, so the caller can report it honestly.
///
/// `already_cached` is the number of names the new index provides that this
/// process has *already served* from somewhere. Those keep the bytes they were
/// given, because the engine holds interior pointers into them that have to
/// stay valid, so the reload applies to them at the next launch and not now.
/// Reporting "reloaded" while the old texture is still on screen is exactly
/// the stub-that-lies failure AGENTS.md forbids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reload {
    pub files: usize,
    pub layers: usize,
    pub already_cached: usize,
}

/// Rebuild the overlay index from the roots as they are on disk now.
///
/// Cheap by construction — this is a directory walk and an `Arc` swap, with
/// nothing remounted and nothing copied, which is the property that makes hot
/// reload worth having at all under interception.
pub fn reload() -> Reload {
    invalidate();
    let index = index();
    let already_cached = MANAGER
        .get()
        .and_then(|m| m.cache.lock().ok().map(|c| index.files.keys().filter(|n| c.contains_key(*n)).count()))
        .unwrap_or(0);
    Reload { files: index.len(), layers: index.layers.len(), already_cached }
}

/// A cheap fingerprint of every registered root: how many files, and the
/// newest modification time across them.
///
/// **Polled rather than inotify'd, and that is the honest trade.** An inotify
/// watch has to be re-armed per directory as subdirectories appear, and
/// getting that wrong presents as a reload that silently stops working. A
/// signature that costs one `stat` per file, taken twice a second while an
/// author is editing, is small enough not to matter and simple enough to be
/// obviously right. It misses a change that leaves both the count and the
/// newest mtime alone — editing a file and restoring its timestamp — and the
/// answer to that is the explicit reload, which is the default anyway.
fn roots_signature() -> (usize, std::time::SystemTime) {
    let mut count = 0usize;
    let mut newest = std::time::SystemTime::UNIX_EPOCH;
    for layer in layers() {
        for (_, path) in walk_root(&layer.root) {
            count += 1;
            if let Ok(modified) = std::fs::metadata(&path).and_then(|m| m.modified()) {
                if modified > newest {
                    newest = modified;
                }
            }
        }
    }
    (count, newest)
}

/// Start the overlay watcher, if `CORDIAL_OVERLAY_WATCH=1`. Returns whether
/// one was started, so the caller can say so rather than assume.
///
/// The thread is deliberately not joined and not stopped: it holds no lock
/// between polls, it does nothing at all when nothing changed, and a
/// development instrument that needs a shutdown protocol is one more thing to
/// get wrong in the path that matters.
pub fn start_watcher() -> bool {
    if !watch_enabled() {
        return false;
    }
    std::thread::Builder::new()
        .name("overlay-watch".into())
        .spawn(|| {
            let mut last = roots_signature();
            loop {
                std::thread::sleep(std::time::Duration::from_millis(500));
                if let Some(report) = watch_tick(&mut last) {
                    // The caveat stated every time rather than once in the
                    // docs, because it is the thing that will otherwise be
                    // read as the reload having failed: the engine holds
                    // pointers into bytes it has already been given, so a name
                    // already served keeps what it was served until the next
                    // launch.
                    println!(
                        "  overlay: reloaded, {} file(s) across {} layer(s); {} already loaded \
                         and unchanged until the next launch",
                        report.files, report.layers, report.already_cached
                    );
                }
            }
        })
        .is_ok()
}

/// One poll: reload if the roots changed since `last`, and report what
/// happened.
///
/// Split out of the thread so the decision is testable without spawning
/// anything or sleeping — the loop around it is a sleep and a `println`, and
/// this is the part that can be wrong.
fn watch_tick(last: &mut (usize, std::time::SystemTime)) -> Option<Reload> {
    let now = roots_signature();
    if now == *last {
        return None;
    }
    *last = now;
    Some(reload())
}

/// Whether to watch the overlay roots for changes. **Off unless
/// `CORDIAL_OVERLAY_WATCH=1`.**
///
/// Watching a directory tree costs wakeups, and a texture pack nobody is
/// editing must not cost a single one — this codebase has a standing rule
/// about paying for nothing. Reload is an explicit call by default; the
/// watcher is what an author turns on while iterating, and if it ever misses
/// an event the fix is to press reload rather than to make the watcher
/// load-bearing.
pub fn watch_enabled() -> bool {
    std::env::var_os("CORDIAL_OVERLAY_WATCH").is_some_and(|v| v != "0")
}

/// The live resolution `Manager::read` uses: one probe into the built index.
fn resolve_overlay(name: &str) -> Option<(OverlaySource, PathBuf)> {
    index().get(name).map(|r| (r.source.clone(), r.path.clone()))
}

/// Which layer currently serves `name` — `"user"`, `"plugin:<id>"`, or `None`
/// meaning the APK itself. Diagnostic only; it reads the current index rather
/// than remembering what was actually served, so it can disagree with an asset
/// that was cached before the stack last changed. That is the same page the
/// running process has been serving since, which is the question worth
/// answering here.
pub fn explain(name: &str) -> Option<String> {
    resolve_overlay(name).map(|(source, _)| source.describe())
}

// ------------------------------------------------------- the filesystem route
//
// The engine does not reach every asset through `AAssetManager`. It is also
// handed `setAssetFolder <cache>/assets/content` and reads through libc, which
// is the route ADR-010 explicitly left out of scope and ADR-021 brings in.
//
// The two routes share this one resolver and one index. They must, or an
// overlay applies to a texture reached one way and not to the same texture
// reached the other, which is a bug nobody would guess from the symptom.
//
// Ground truth on what has to be covered, from the engine's own dynamic symbol
// table rather than from reasoning about it: `libroblox.so` imports `access`,
// `fdopen`, `fopen`, `fstat`, `ftruncate`, `lstat`, `mmap`, `open`, `opendir`,
// `readlink`, `realpath` and `stat`. **There is no `openat`**, no `fstatat`,
// no `statx` and no `open64`, so the classic dirfd-relative bypass has nothing
// to bypass through on this build — a fact to re-check whenever the build
// moves, which `docs/analysis/undefined-symbols.tsv` makes a one-line diff.
//
// `fstat` needs no resolver and that is a result rather than an omission: it
// takes an fd, which is the fd our `open` already returned, pointing at the
// overlay file itself, so it reports the overlay's size because it is looking
// at the overlay. The size-versus-bytes mismatch everyone warns about needs
// the two to come from different files, and it can only arise through the
// *path-taking* size and existence calls — `stat`, `lstat` and `access` —
// every one of which is already in `native/system_paths.cpp`'s table beside
// `open`. Hence the invariant that table enforces: every path-taking function
// in it consults this resolver, or none of them do.

/// The extracted asset root the engine was given, if it was given one.
///
/// Set by the loader when it hands the engine `assetFolderPath`. Until it is
/// set, `resolve_asset_path` redirects nothing at all — failing closed to the
/// original path, which for this route is not a stylistic preference: the
/// first thing the engine resolves by real path is `ssl/cacert.pem`, and a
/// resolver that guesses wrong there produces a TLS failure three layers from
/// anything mentioning certificates. This project has already paid for that
/// once.
static ASSET_ROOT: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Record the directory `assetFolderPath` points at — the extraction root,
/// **not** its `content` subdirectory, because overlay names are relative to
/// `assets/` and `content/…` is part of the name.
pub fn set_asset_root(dir: &Path) {
    if let Ok(mut held) = ASSET_ROOT.lock() {
        *held = dir.canonicalize().ok().or_else(|| Some(dir.to_path_buf()));
    }
}

/// Whether these `open` flags mean the caller intends to write.
///
/// `O_WRONLY` is 1 and `O_RDWR` is 2 on every Linux ABI Cordial runs on, and
/// the creating and truncating flags are checked beside them so an
/// `O_RDONLY|O_CREAT` cannot slip past as a read.
pub fn is_write_intent(flags: i32) -> bool {
    const O_WRONLY: i32 = 0o1;
    const O_RDWR: i32 = 0o2;
    const O_CREAT: i32 = 0o100;
    const O_TRUNC: i32 = 0o1000;
    const O_APPEND: i32 = 0o2000;
    let access = flags & 0o3;
    access == O_WRONLY || access == O_RDWR || flags & (O_CREAT | O_TRUNC | O_APPEND) != 0
}

/// Where a real filesystem path under the extracted asset tree should actually
/// be read from, or `None` to use the path unchanged.
///
/// **Writes are never redirected**, and this is decided here rather than left
/// to be discovered from a corrupted cache. ADR-010's entire claim is that
/// nothing is written into the APK or into anything extracted from it, so
/// handing a writable fd to a plugin's file would make an overlay a place the
/// engine can scribble — neither non-destructive nor anything the plugin's
/// author agreed to. Refusing the open outright would break an engine write to
/// a path that merely collides with an overlay name, and copy-on-write needs
/// the manifest of what was copied that ADR-010 declined to build. So reads
/// resolve to the overlay and writes go to the original.
///
/// Anything outside the asset root is not an asset and is left alone. The
/// overlay must never become a general filesystem redirect; that is the same
/// line ADR-007 draws between an effect and a channel.
pub fn resolve_asset_path(path: &str, write: bool) -> Option<PathBuf> {
    if write {
        return None;
    }
    let root = ASSET_ROOT.lock().ok()?.clone()?;
    let candidate = Path::new(path);
    // Not canonicalised first: the file being looked for frequently does not
    // exist yet under the extraction root, and `canonicalize` on a missing
    // path fails. Lexical containment is enough because the name is then put
    // through the index, which only ever holds names walked from inside a
    // root — an escaping name simply misses.
    let rel = candidate.strip_prefix(&root).ok()?;
    let name = rel.components().filter_map(|c| c.as_os_str().to_str()).collect::<Vec<_>>().join("/");
    if name.is_empty() || rel.components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
        return None;
    }
    index().get(&name).map(|r| r.path.clone())
}

/// The C ABI `native/system_paths.cpp` calls from each of its path-taking
/// functions.
///
/// **Not yet called.** The table in `cordial_system_symbols` holds `stat`,
/// `lstat`, `access`, `opendir`, `realpath`, `readlink`, `fopen`, `statvfs`
/// and `open`, and every one of them must route through here together — the
/// invariant is that they all consult one resolver or none of them do, because
/// a size call answering about the original while `open` answers with the
/// overlay is the mismatch that truncates a texture. Wiring it is a change to
/// that file, which was held by another agent when this was written; the Rust
/// half is complete and tested so the C++ half is one call per function.
///
/// Writes `out` as a NUL-terminated path and returns 1 when the caller should
/// use it, 0 when it should use the path it was given. A buffer too small
/// returns 0 rather than a truncated path, because a truncated path names a
/// different file and would be worse than no redirect at all.
///
/// # Safety
///
/// `path` is a NUL-terminated C string; `out` points at `out_len` writable
/// bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cordial_overlay_resolve(
    path: *const c_char,
    for_write: c_int,
    out: *mut c_char,
    out_len: usize,
) -> c_int {
    // Before the lookup, not after: resolving records the request, and a call
    // with nowhere to put an answer should not leave one behind.
    if out.is_null() || out_len == 0 {
        return 0;
    }
    // SAFETY: the function's own contract requires `path` be NUL-terminated
    // when it is not null, and a null one is refused by the helper.
    let Some(name) = (unsafe { borrow_request(path) }) else { return 0 };
    let Some(answer) = redirect_bytes(&name.to_string_lossy(), for_write != 0) else {
        return 0;
    };
    // A buffer too small is refused whole rather than truncated, which the
    // helper guarantees: a truncated path names a different file.
    c_int::from(write_response(&answer, out, out_len))
}

/// The redirect for `name` as NUL-terminated bytes, or `None` to use the path
/// as given. Everything `cordial_overlay_resolve` does that is not a pointer.
fn redirect_bytes(name: &str, for_write: bool) -> Option<Vec<u8>> {
    let resolved = resolve_asset_path(name, for_write)?;
    let mut bytes = resolved.as_os_str().as_encoded_bytes().to_vec();
    bytes.push(0);
    Some(bytes)
}

// -------------------------------------------------------------- the recorder
//
// Every distinct name the engine asks for, with what answered it. One cold
// launch plus one game join is the ground-truth list of what this build
// actually reads, which is the missing half of the Windows-to-Android path
// mapping and is the thing both orphan signals are computed against.
//
// Always on, and a set rather than a log, because that is what makes it free:
// a name already seen costs one hash lookup and no allocation. The stderr
// tracing under `CORDIAL_TRACE_ASSETS=1` is unchanged and separate — it is a
// stream, this is a summary, and only the summary can answer "what was never
// asked for".

/// What answered a request. Kept apart because a miss is a different fact from
/// a hit and reporting them as one is how a broken overlay looks healthy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Served {
    Overlay(OverlaySource),
    Apk,
    Missing,
}

impl Served {
    pub fn describe(&self) -> String {
        match self {
            Served::Overlay(source) => source.describe(),
            Served::Apk => "apk".into(),
            Served::Missing => "missing".into(),
        }
    }
}

static REQUESTED: Mutex<Option<BTreeMap<String, Served>>> = Mutex::new(None);

fn record(name: &str, served: Served) {
    if let Ok(mut held) = REQUESTED.lock() {
        held.get_or_insert_with(BTreeMap::new).insert(name.to_string(), served);
    }
}

/// Every distinct asset name requested so far, with what answered it.
pub fn requested() -> BTreeMap<String, Served> {
    REQUESTED.lock().ok().and_then(|h| h.clone()).unwrap_or_default()
}

/// Where the request trace is written: `$XDG_DATA_HOME/cordial/asset-trace.log`.
///
/// Under `XDG_DATA_HOME` rather than the cache, because it is a measurement
/// somebody took and not something Cordial can regenerate — and because
/// AGENTS.md's instruction to give a run its own data root then redirects it
/// with everything else, which is what keeps two agents' traces apart.
pub fn trace_path() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(std::env::temp_dir)
        .join("cordial/asset-trace.log")
}

/// Write the request trace, one `name<TAB>source` line per distinct name,
/// sorted. Sorted on the way out so `sort -u` on it is idempotent and two
/// runs can be diffed without preprocessing.
pub fn write_trace(path: &Path) -> std::io::Result<usize> {
    let requested = requested();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut text = String::new();
    for (name, served) in &requested {
        text.push_str(name);
        text.push('\t');
        text.push_str(&served.describe());
        text.push('\n');
    }
    std::fs::write(path, text)?;
    Ok(requested.len())
}

// ---------------------------------------------------------------- the orphans
//
// Two signals that mean different things and must never be reported as one.
//
// *Stale* is an overlay file with no counterpart anywhere in the APK: the name
// is wrong, or the build removed it. It can never apply. This is the signal
// that catches a Bloxstrap mod shipping `content/sounds/ouch.ogg` when this
// build reads `content/sounds/oof.ogg` — checked against the real archive, the
// most famous mod there is misses by one word and says nothing.
//
// *Unrequested* is an overlay file that does exist in the APK but that the
// engine never asked for in this session. It did not apply *today*, possibly
// because the feature it decorates was never opened, possibly because the run
// was sixty seconds long.
//
// Stale is a defect. Unrequested is an observation. A report that gives a
// single count of "orphans" has merged a certainty with a maybe.

/// An overlay file that matches nothing in the APK's asset tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stale {
    pub name: String,
    pub source: OverlaySource,
}

/// Every name in the APK's `assets/` tree, relative to it — the set an overlay
/// is diffed against.
///
/// Reads the zip's central directory only, so it costs a directory read rather
/// than a decompression of ninety-odd megabytes.
pub fn apk_asset_names() -> Result<BTreeSet<String>, String> {
    let manager = MANAGER.get().ok_or("no APK is set")?;
    let zip = manager.archive().ok_or_else(|| format!("cannot read {}", manager.apk.display()))?;
    Ok(zip
        .file_names()
        .filter_map(|n| n.strip_prefix("assets/"))
        .filter(|n| !n.is_empty() && !n.ends_with('/'))
        .map(str::to_string)
        .collect())
}

/// The Roblox client version this APK is, as `2.734.917`, or `None`.
///
/// **A scan, not a parser, and it says `None` rather than guessing.** Android's
/// `AndroidManifest.xml` inside an APK is binary AXML, whose string pool is
/// UTF-16; `versionName` is one of the strings in it. Writing an AXML parser to
/// read one field would be a great deal of code to maintain against a format
/// that has changed before, so this scans the pool for something shaped like a
/// Roblox version and takes the first.
///
/// It exists because "7 files no longer match anything" is not a useful
/// sentence without a version beside it — the same files were fine last month,
/// and the user's next question is always "since when". A wrong version there
/// would be worse than none, which is why an ambiguous scan returns `None` and
/// the caller falls back to naming the file.
pub fn client_version() -> Option<String> {
    let manager = MANAGER.get()?;
    let mut zip = manager.archive()?;
    let mut entry = zip.by_name("AndroidManifest.xml").ok()?;
    let mut bytes = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut bytes).ok()?;

    // UTF-16LE with the high byte zero for ASCII, so the ASCII characters sit
    // at even offsets with NULs between them. Pulling those out gives a
    // readable ribbon of the whole pool without decoding it properly.
    let ribbon: String = bytes
        .chunks_exact(2)
        .filter(|c| c[1] == 0)
        .map(|c| if c[0].is_ascii_graphic() { c[0] as char } else { '\n' })
        .collect();
    ribbon.split('\n').find(|token| is_client_version(token)).map(str::to_string)
}

/// Whether a token looks like a Roblox client version: three dotted numeric
/// parts with a three-digit middle, which is the shape every published Android
/// build has had. Deliberately narrow — a looser pattern matches a library
/// version out of the same string pool and reports it as the client's.
fn is_client_version(token: &str) -> bool {
    let parts: Vec<&str> = token.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        && parts[1].len() == 3
}

/// Overlay files that match nothing in `apk`. Sorted, so a report is stable.
pub fn stale(apk: &BTreeSet<String>) -> Vec<Stale> {
    let index = index();
    let mut out: Vec<Stale> = index
        .files
        .iter()
        .filter(|(name, _)| !apk.contains(*name))
        .map(|(name, r)| Stale { name: name.clone(), source: r.source.clone() })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Overlay files the engine never asked for in this session.
///
/// Weaker than [`stale`] on purpose, and its caveat has to travel with it
/// wherever it is shown: a run that never joins an experience leaves almost
/// everything unrequested. It is a strong signal after a real session and a
/// misleading one after a smoke test.
pub fn unrequested() -> Vec<Stale> {
    let asked = requested();
    let index = index();
    let mut out: Vec<Stale> = index
        .files
        .iter()
        .filter(|(name, _)| !asked.contains_key(*name))
        .map(|(name, r)| Stale { name: name.clone(), source: r.source.clone() })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// A one-line summary per plugin of what its overlay is not doing, in the
/// words a user can act on.
///
/// Names the client build, because "no longer matches anything" is only
/// meaningful against a version — the same file was fine last month.
pub fn stale_report(apk: &BTreeSet<String>, build: &str) -> Vec<String> {
    let mut per_source: BTreeMap<String, usize> = BTreeMap::new();
    for orphan in stale(apk) {
        *per_source.entry(orphan.source.describe()).or_default() += 1;
    }
    per_source
        .into_iter()
        .map(|(source, count)| {
            let (files, verb) = if count == 1 { ("file", "matches") } else { ("files", "match") };
            format!("{source}: {count} {files} no longer {verb} anything in client {build}")
        })
        .collect()
}

/// Which layer beat which, for every name more than one layer offered.
///
/// `user:my-fonts wins over plugin:retro-ui   content/fonts/…`
pub fn shadow_report() -> Vec<String> {
    let index = index();
    index
        .shadowing()
        .into_iter()
        .map(|(name, resolved)| {
            let losers: Vec<String> = resolved.shadowed.iter().map(OverlaySource::describe).collect();
            format!("{} wins over {}   {name}", resolved.source.describe(), losers.join(", "))
        })
        .collect()
}

/// Extract `assets/` to a real directory and return its path.
///
/// Not everything in the engine reads assets through `AAssetManager`. Its HTTP
/// stack is curl, and curl's `CURLOPT_CAINFO` takes a **filesystem path** — it
/// cannot be handed a zip entry or a pointer. Roblox ships its CA bundle as
/// `assets/ssl/cacert.pem` precisely because the Android app extracts assets to
/// a real directory and gives the engine that directory.
///
/// Cordial was passing the `.apk` file itself as `assetFolderPath`, so every
/// path the engine built from it (`<assetFolder>/ssl/cacert.pem`) named a file
/// inside a file and could not be opened. TLS verification then has no roots,
/// which fails the client-settings fetch, which fails the flag set — three
/// layers away from anything that mentions certificates.
///
/// Extraction is skipped when the destination is **stamped with this exact
/// APK**, so repeat launches pay for it once and a new Roblox build pays for
/// it again.
///
/// **Two defects fixed here on 2026-08-22, both of the kind this project calls
/// its most expensive.**
///
/// *It had no staleness check at all.* The test was `out.exists()`, per file,
/// so a new APK left every previously-extracted asset exactly as it was and
/// the engine ran the new build against the old build's content. That is the
/// same silent version mismatch `cordial_update::cache`'s own module doc was
/// written for, one directory across — the shell had it fixed for
/// `libroblox.so` and this had not. The fix is that module, not a second
/// notion of "is this current": two independent answers to that question is
/// how one of them quietly stops being true, and `install.rs` already has the
/// tests pinning both directions.
///
/// *It had no completion marker.* An extraction interrupted half way left a
/// partial tree that the per-file existence check then read as finished, and
/// nothing anywhere would ever have said so. The stamp is now the completion
/// marker: it is written after the last file and nowhere else, so a partial
/// tree is unstamped, and unstamped is not current.
///
/// Each file is also written through a temporary and renamed into place, so no
/// individual asset can be observed half-written — the engine `mmap`s some of
/// these and a truncated read is a texture rendered as garbage rather than an
/// error anybody sees.
///
/// **The tree is overwritten in place rather than swapped in.** Same choice
/// `install.rs` makes and for the same reason it records: deleting first would
/// leave a user with nothing at all if the extraction then failed, and an
/// unstamped tree simply re-extracts next launch, which is slow rather than
/// wrong.
///
/// **A gap that remains, named rather than left to be discovered.** A file
/// present in the old APK and absent from the new one is not removed, because
/// nothing here walks the destination to diff it. The filesystem route would
/// still serve that orphan. It has not been seen and removing a cache
/// directory's contents is a change that wants its own test rather than a
/// clause in this one, but it is the honest remaining hole in "the extracted
/// tree matches this APK".
pub fn extract_to(dir: &Path) -> Result<PathBuf, String> {
    let manager = MANAGER.get().ok_or("no APK is set")?;
    if cordial_update::cache::is_current(dir, &manager.apk) {
        return Ok(dir.to_path_buf());
    }
    let zip =
        manager.archive().ok_or_else(|| format!("cannot read {}", manager.apk.display()))?;
    extract_archive_to(dir, &manager.apk, zip)
}

/// The half of [`extract_to`] that does not need the process-wide `MANAGER`.
///
/// Split out for the reason `merge` is split from `apply_overrides` and
/// `build_index` from `index`: `MANAGER` is a `OnceLock` set at most once per
/// process, so a test that went through the public entry point could exercise
/// exactly one APK per test binary. Generic over the reader so a test can hand
/// it an archive built in memory.
fn extract_archive_to<R: Read + Seek>(
    dir: &Path,
    apk: &Path,
    mut zip: zip::ZipArchive<R>,
) -> Result<PathBuf, String> {
    // Said out loud, because until now this was silent either way and a user
    // whose first launch after an update took an extra second had nothing to
    // attribute it to.
    println!("  assets: {} is not extracted from {}; extracting", dir.display(), apk.display());

    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut written = 0usize;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        let Some(name) = entry.enclosed_name().map(|p| p.to_path_buf()) else {
            // `enclosed_name` is None for entries that escape the destination
            // (`../`, absolute paths). Skipping them is the whole defence
            // against a zip-slip write outside `dir`.
            continue;
        };
        let Ok(rel) = name.strip_prefix("assets") else {
            continue;
        };
        if entry.is_dir() {
            continue;
        }
        let out = dir.join(rel);
        // No `out.exists()` skip any more. That skip was the staleness bug:
        // once the APK has changed, an existing file with the right name is
        // the *wrong* file, and keeping it is the whole failure.
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        write_atomically(&out, &bytes)?;
        written += 1;
    }

    // Last, and only on success. Everything above can fail and leave a tree
    // that is merely unstamped, which the check at the top of this function
    // treats as "extract again" -- the recoverable direction.
    cordial_update::cache::write_stamp(dir, apk)
        .map_err(|e| format!("extracted {written} assets but could not stamp {}: {e}", dir.display()))?;
    println!("  assets: extracted {written} files into {}", dir.display());
    Ok(dir.to_path_buf())
}

/// Write `bytes` to `out` through a temporary in the same directory.
///
/// Same-directory so the rename is within one filesystem and therefore atomic;
/// a temporary in `/tmp` would be a copy across a mount on this host, which is
/// not. The temporary carries the pid so two processes extracting into one
/// cache cannot truncate each other's half-written file -- profiles are locked
/// one instance at a time (ADR-012) but the *cache* is machine-wide and shared
/// between them.
fn write_atomically(out: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = out.with_extension(format!("cordial-{}", std::process::id()));
    std::fs::write(&tmp, bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
    match std::fs::rename(&tmp, out) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(format!("{}: {e}", out.display()))
        }
    }
}

// ------------------------------------------------------------------- the API

unsafe extern "C" fn asset_manager_from_java(_env: *mut c_void, _obj: *mut c_void) -> *mut c_void {
    super::trace(format_args!("AAssetManager_fromJava"));
    // There is one asset manager per process and the Java object is Cordial's
    // own, so there is nothing to look up. Returning a non-null token matters:
    // Roblox checks it.
    MANAGER.get().map_or(std::ptr::null_mut(), |m| m as *const Manager as *mut c_void)
}

unsafe extern "C" fn asset_manager_open(
    _mgr: *mut c_void,
    filename: *const c_char,
    _mode: c_int,
) -> *mut c_void {
    let Some(manager) = MANAGER.get() else {
        eprintln!("[asset] open before an APK was set — pass --apk");
        return std::ptr::null_mut();
    };
    // SAFETY: the API contract is a NUL-terminated path.
    let Some(name) = (unsafe { borrow_request(filename) }) else {
        return std::ptr::null_mut();
    };
    open_asset(manager, &name.to_string_lossy()).map_or(std::ptr::null_mut(), into_handle)
}

/// Not an error worth failing on: Android's asset manager returns null for a
/// missing asset and callers probe for optional files. `read` already traced
/// the miss (under `CORDIAL_TRACE_ASSETS=1`) with the name.
fn open_asset(manager: &Manager, name: &str) -> Option<Asset> {
    manager.read(name).map(|bytes| Asset { bytes })
}

unsafe extern "C" fn asset_get_buffer(asset: *mut c_void) -> *const c_void {
    // SAFETY: `asset` is null or came from asset_manager_open and has not been
    // closed.
    unsafe { handle_ref::<Asset>(asset) }.map_or(std::ptr::null(), |a| a.bytes.as_ptr() as *const c_void)
}

unsafe extern "C" fn asset_get_length(asset: *mut c_void) -> i64 {
    // SAFETY: as above.
    unsafe { handle_ref::<Asset>(asset) }.map_or(0, |a| a.bytes.len() as i64)
}

unsafe extern "C" fn asset_close(asset: *mut c_void) {
    // SAFETY: `asset` is null or came from asset_manager_open and is closed
    // exactly once, per the API contract.
    unsafe { drop_handle::<Asset>(asset) };
}

/// Some callers want a file descriptor rather than a buffer — Android can give
/// one because assets are stored uncompressed at a known offset in the APK.
/// Cordial's are decompressed in memory, so this hands back a `memfd` holding
/// the same bytes, which behaves the same for reading.
unsafe extern "C" fn asset_open_file_descriptor(
    asset: *mut c_void,
    out_start: *mut i64,
    out_length: *mut i64,
) -> c_int {
    // SAFETY: `asset` is null or came from asset_manager_open.
    let Some(a) = (unsafe { handle_ref::<Asset>(asset) }) else {
        return -1;
    };
    let Some(fd) = memfd_with(a.bytes) else {
        return -1;
    };
    // SAFETY: the caller's out-parameters, each valid or null.
    unsafe {
        store_out(out_start, 0);
        store_out(out_length, a.bytes.len() as i64);
    }
    fd.into_raw_fd()
}

/// A fresh `memfd` holding exactly `bytes`, positioned at the start.
///
/// A short `write` is retried by `write_all`; the first version made one call
/// and returned -1 on a short count without closing the descriptor, which is a
/// leak for every large asset that hit it. Dropping the `File` on any failure
/// closes it.
fn memfd_with(bytes: &[u8]) -> Option<OwnedFd> {
    // SAFETY: a literal name and no flags.
    let raw = unsafe { libc::memfd_create(c"cordial-asset".as_ptr(), 0) };
    if raw < 0 {
        return None;
    }
    // SAFETY: `raw` is a descriptor this function just created and nothing else
    // owns.
    let mut file = File::from(unsafe { OwnedFd::from_raw_fd(raw) });
    file.write_all(bytes).ok()?;
    file.seek(SeekFrom::Start(0)).ok()?;
    Some(file.into())
}

// ------------------------------------------------------------- directories
//
// The Quest build's OpenXR loader imports `AAssetManager_openDir` and its two
// companions. The bionic linker binds every import when a library loads,
// functions as much as data, and the loader is in `libroblox.so`'s
// `DT_NEEDED`, so without these the engine did not load at all. The phone
// build never imported them.

/// An open `AAssetDir`: the file names directly inside one asset directory, and
/// how far through them the caller has read.
///
/// `CString`s held here so that the pointer `AAssetDir_getNextFileName` returns
/// stays valid until the next call or `close`, which is the NDK's promise.
struct AssetDir {
    names: Vec<std::ffi::CString>,
    next: usize,
}

impl AssetDir {
    /// The next name, or null at the end. The pointer is into `self`, so it
    /// stays valid until the next call or `close`, as the NDK promises.
    fn next_name(&mut self) -> *const c_char {
        match self.names.get(self.next) {
            Some(name) => {
                self.next += 1;
                name.as_ptr()
            }
            None => std::ptr::null(),
        }
    }
}

/// The file names directly inside `assets/<dir>/`, sorted.
///
/// Files only, and only one level: Android's `AAssetDir_getNextFileName` is
/// documented to skip subdirectories, and a zip has no directory entries it
/// can be relied on to carry anyway, so a subdirectory shows up here only as a
/// name with a further `/` in it and is dropped. Sorted because Android's
/// `AssetDir` is a sorted vector; nothing is known to depend on the order, but
/// a listing that changed order between two APKs with the same contents would
/// be one more thing to rule out.
///
/// `dir` is taken with or without surrounding slashes, and the empty string is
/// the root of `assets/`. Overlays are not consulted: they replace files the APK
/// already has and are not known to add any, so the APK's index is the listing.
fn list_dir<'a>(entries: impl Iterator<Item = &'a str>, dir: &str) -> Vec<String> {
    let dir = dir.trim_matches('/');
    let prefix = if dir.is_empty() {
        "assets/".to_string()
    } else {
        format!("assets/{dir}/")
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.strip_prefix(&prefix))
        .filter(|rest| !rest.is_empty() && !rest.contains('/'))
        .map(str::to_string)
        .collect();
    names.sort();
    names.dedup();
    names
}

unsafe extern "C" fn asset_manager_open_dir(_mgr: *mut c_void, dir_name: *const c_char) -> *mut c_void {
    let Some(manager) = MANAGER.get() else {
        eprintln!("[asset] openDir before an APK was set — pass --apk");
        return std::ptr::null_mut();
    };
    // SAFETY: the API contract is a NUL-terminated path.
    let Some(dir) = (unsafe { borrow_request(dir_name) }) else {
        return std::ptr::null_mut();
    };
    open_dir(manager, &dir.to_string_lossy()).map_or(std::ptr::null_mut(), into_handle)
}

/// NULL is the documented failure value, and an unreadable APK is a failure;
/// an empty listing would claim the directory is merely empty. A directory the
/// APK does not have is an empty listing rather than NULL, which is what
/// Android's AssetManager gives for one too (INFERRED from its `openDir`
/// merging whatever it finds across asset paths, not measured on a device).
fn open_dir(manager: &Manager, dir: &str) -> Option<AssetDir> {
    let zip = manager.archive()?;
    let names: Vec<std::ffi::CString> = list_dir(zip.file_names(), dir)
        .into_iter()
        .filter_map(|n| std::ffi::CString::new(n).ok())
        .collect();
    if trace_assets_enabled() {
        eprintln!("[asset] openDir: {dir:?} -> {} files", names.len());
    }
    Some(AssetDir { names, next: 0 })
}

unsafe extern "C" fn asset_dir_get_next_file_name(dir: *mut c_void) -> *const c_char {
    // SAFETY: `dir` is null or came from asset_manager_open_dir and has not been
    // closed; the engine reads one directory from one thread.
    unsafe { handle_mut::<AssetDir>(dir) }.map_or(std::ptr::null(), AssetDir::next_name)
}

unsafe extern "C" fn asset_dir_close(dir: *mut c_void) {
    // SAFETY: `dir` is null or came from asset_manager_open_dir and is closed
    // exactly once, per the API contract.
    unsafe { drop_handle::<AssetDir>(dir) };
}

/// Everything this module provides, for the symbol table.
pub fn overrides() -> Vec<(&'static str, *mut c_void)> {
    macro_rules! f {
        ($name:literal, $fn:expr) => {
            ($name, $fn as *const () as *mut c_void)
        };
    }
    vec![
        f!("AAssetManager_fromJava", asset_manager_from_java),
        f!("AAssetManager_open", asset_manager_open),
        f!("AAsset_getBuffer", asset_get_buffer),
        f!("AAsset_getLength", asset_get_length),
        f!("AAsset_close", asset_close),
        f!("AAsset_openFileDescriptor", asset_open_file_descriptor),
        f!("AAssetManager_openDir", asset_manager_open_dir),
        f!("AAssetDir_getNextFileName", asset_dir_get_next_file_name),
        f!("AAssetDir_close", asset_dir_close),
    ]
}

/// Open an asset through the same entry points Roblox uses and report its size.
///
/// Exercising the C API rather than `Manager::read` is the point: it is the
/// pointer handling either side of the FFI boundary that is worth proving, not
/// the zip lookup.
pub fn probe(name: &str) -> Result<usize, String> {
    let c = std::ffi::CString::new(name).map_err(|e| e.to_string())?;
    let mgr = unsafe { asset_manager_from_java(std::ptr::null_mut(), std::ptr::null_mut()) };
    if mgr.is_null() {
        return Err("no APK configured".into());
    }
    let asset = unsafe { asset_manager_open(mgr, c.as_ptr(), 0) };
    if asset.is_null() {
        return Err(format!("asset not found: {name}"));
    }
    let len = unsafe { asset_get_length(asset) as usize };
    let buf = unsafe { asset_get_buffer(asset) };
    if buf.is_null() {
        unsafe { asset_close(asset) };
        return Err("asset opened but its buffer is null".into());
    }
    // Touch the first and last byte: a length without readable memory behind it
    // is exactly the failure this probe exists to catch.
    // SAFETY: `buf` points at `len` bytes owned by the asset, still open.
    let bytes = unsafe { std::slice::from_raw_parts(buf as *const u8, len) };
    let checksum = bytes.first().copied().unwrap_or(0) as usize
        + bytes.last().copied().unwrap_or(0) as usize;
    std::hint::black_box(checksum);
    unsafe { asset_close(asset) };
    Ok(len)
}

#[cfg(test)]
mod overlay_tests {
    use super::*;
    use std::ffi::CStr;

    /// The global overlay stack, the built index and the request recorder are
    /// all process-wide, and cargo runs this file's tests on several threads.
    /// Every test that touches one of them takes this first; the ones that
    /// only build an index from a local slice do not need it.
    static GLOBAL: Mutex<()> = Mutex::new(());

    /// Write `contents` at `dir/rel`, creating whatever directories are needed
    /// to mirror the APK's own tree structure.
    fn write(dir: &Path, rel: &str, contents: &[u8]) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cordial-overlay-test-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // ------------------------------------------------- extraction and the zip

    /// A zip with `assets/<name>` entries, in memory.
    ///
    /// `Stored` rather than `Deflated` so the test does not depend on which
    /// compression features the crate is built with -- the thing under test is
    /// the extraction, not the codec.
    fn apk_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write;
        let mut out = std::io::Cursor::new(Vec::new());
        let mut w = zip::ZipWriter::new(&mut out);
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, bytes) in entries {
            w.start_file(*name, opts).unwrap();
            w.write_all(bytes).unwrap();
        }
        w.finish().unwrap();
        out.into_inner()
    }

    fn archive_of(bytes: &[u8]) -> zip::ZipArchive<std::io::Cursor<Vec<u8>>> {
        zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).unwrap()
    }

    /// Stand-in for a real APK on disk, so `cordial_update::cache` has
    /// something to stat. Its size and mtime are what the stamp is made of.
    fn fake_apk(dir: &Path, name: &str, contents: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn a_directory_listing_is_its_files_only_one_level_deep() {
        let apk = apk_with(&[
            ("assets/openxr/1/manifest.json", b"{}"),
            ("assets/openxr/1/deeper/skip.json", b"{}"),
            ("assets/openxr/other.json", b"{}"),
            ("assets/root.txt", b"x"),
            ("lib/arm64-v8a/libroblox.so", b"not an asset"),
        ]);
        let zip = archive_of(&apk);
        assert_eq!(list_dir(zip.file_names(), "openxr/1"), ["manifest.json"]);
        // Surrounding slashes are the caller's business, not a different dir.
        assert_eq!(list_dir(zip.file_names(), "/openxr/1/"), ["manifest.json"]);
        // `deeper/` is a subdirectory and `1/` is one too; neither is a file.
        assert_eq!(list_dir(zip.file_names(), "openxr"), ["other.json"]);
        assert_eq!(list_dir(zip.file_names(), ""), ["root.txt"]);
        assert!(list_dir(zip.file_names(), "absent").is_empty());
    }

    // ------------------------------------------------- the C entry points

    /// An `AAsset*` holding `bytes`, as `asset_manager_open` would hand out.
    fn handle_to(bytes: &'static [u8]) -> *mut c_void {
        into_handle(Asset { bytes })
    }

    #[test]
    fn the_asset_entry_points_answer_a_null_handle_with_their_failure_value() {
        // The engine probes; a null `AAsset*` is the miss it already handles.
        assert!(unsafe { asset_get_buffer(std::ptr::null_mut()) }.is_null());
        assert_eq!(unsafe { asset_get_length(std::ptr::null_mut()) }, 0);
        assert_eq!(asset_open_file_descriptor(std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()), -1);
        unsafe { asset_close(std::ptr::null_mut()) };
    }

    #[test]
    fn an_open_asset_reports_its_bytes_and_closes() {
        let a = handle_to(b"hello");
        assert_eq!(unsafe { asset_get_length(a) }, 5);
        let p = unsafe { asset_get_buffer(a) };
        // SAFETY: five bytes owned by the asset, which is still open.
        assert_eq!(unsafe { std::slice::from_raw_parts(p as *const u8, 5) }, b"hello");
        unsafe { asset_close(a) };
    }

    #[test]
    fn a_file_descriptor_holds_the_asset_from_its_first_byte() {
        let a = handle_to(b"0123456789");
        let (mut start, mut length) = (-1i64, -1i64);
        let fd = asset_open_file_descriptor(a, &mut start, &mut length);
        assert!(fd >= 0);
        assert_eq!((start, length), (0, 10));
        // SAFETY: a descriptor this call just handed over, owned from here on.
        let mut file = File::from(unsafe { OwnedFd::from_raw_fd(fd) });
        let mut got = Vec::new();
        file.read_to_end(&mut got).unwrap();
        assert_eq!(got, b"0123456789", "the descriptor must be positioned at the start");
        // Both out-parameters are optional.
        let again = asset_open_file_descriptor(a, std::ptr::null_mut(), std::ptr::null_mut());
        assert!(again >= 0);
        // SAFETY: as above.
        drop(unsafe { OwnedFd::from_raw_fd(again) });
        asset_close(a);
    }

    #[test]
    fn a_large_asset_survives_the_trip_through_a_memfd() {
        // Large enough that a single `write` is allowed to be short, which the
        // first version of this treated as failure and leaked the descriptor.
        let big: Vec<u8> = (0..16 * 1024 * 1024u32).map(|i| (i % 251) as u8).collect();
        let fd = memfd_with(&big).expect("a memfd");
        let mut file = File::from(fd);
        let mut got = Vec::new();
        file.read_to_end(&mut got).unwrap();
        assert!(got == big, "the bytes read back are not the bytes written");
    }

    #[test]
    fn an_overlay_redirect_is_nul_terminated_and_refused_when_it_does_not_fit() {
        let _guard = GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
        isolate("resolve-c");
        let root = scratch("resolve-c-assets");
        std::fs::create_dir_all(root.join("content")).unwrap();
        let overlay = scratch("resolve-c-overlay");
        write(&overlay, "content/a.bin", b"overlay");
        register_plugin_root("resolver", overlay);
        set_asset_root(&root);
        let target = std::ffi::CString::new(root.join("content/a.bin").to_string_lossy().as_bytes()).unwrap();

        let mut buf = vec![0x7fu8; 4096];
        // SAFETY: `target` is NUL-terminated and `buf` is 4096 writable bytes.
        let hit = unsafe { cordial_overlay_resolve(target.as_ptr(), 0, buf.as_mut_ptr() as *mut c_char, buf.len()) };
        assert_eq!(hit, 1);
        let end = buf.iter().position(|b| *b == 0).expect("a NUL terminator");
        assert!(String::from_utf8_lossy(&buf[..end]).ends_with("content/a.bin"));

        // Too small: refused, and not a byte of it written.
        let mut small = vec![0x7fu8; 8];
        // SAFETY: as above with an 8-byte buffer.
        let miss = unsafe { cordial_overlay_resolve(target.as_ptr(), 0, small.as_mut_ptr() as *mut c_char, small.len()) };
        assert_eq!(miss, 0);
        assert_eq!(small, vec![0x7fu8; 8], "a refused answer must not be partly written");

        // A write is not redirected, and the degenerate arguments are no-ops.
        // SAFETY: as above.
        unsafe {
            assert_eq!(cordial_overlay_resolve(target.as_ptr(), 1, buf.as_mut_ptr() as *mut c_char, buf.len()), 0);
            assert_eq!(cordial_overlay_resolve(std::ptr::null(), 0, buf.as_mut_ptr() as *mut c_char, buf.len()), 0);
            assert_eq!(cordial_overlay_resolve(target.as_ptr(), 0, std::ptr::null_mut(), 16), 0);
            assert_eq!(cordial_overlay_resolve(target.as_ptr(), 0, buf.as_mut_ptr() as *mut c_char, 0), 0);
        }
        unregister_plugin_root("resolver");
    }

    #[test]
    fn get_next_file_name_walks_the_listing_once_then_answers_null() {
        let names = ["a.json", "b.json"]
            .iter()
            .map(|n| std::ffi::CString::new(*n).unwrap())
            .collect();
        let dir = Box::into_raw(Box::new(AssetDir { names, next: 0 })) as *mut c_void;
        let mut seen = Vec::new();
        loop {
            let p = asset_dir_get_next_file_name(dir);
            if p.is_null() {
                break;
            }
            // SAFETY: a pointer into the listing, which is still open.
            seen.push(unsafe { CStr::from_ptr(p) }.to_str().unwrap().to_string());
        }
        assert_eq!(seen, ["a.json", "b.json"]);
        // Exhausted stays exhausted rather than wrapping round.
        assert!(asset_dir_get_next_file_name(dir).is_null());
        asset_dir_close(dir);
        // A null handle is refused rather than dereferenced.
        assert!(asset_dir_get_next_file_name(std::ptr::null_mut()).is_null());
        asset_dir_close(std::ptr::null_mut());
    }

    #[test]
    fn a_clone_of_the_apk_reader_has_its_own_position() {
        // The whole safety argument for sharing one parsed central directory
        // is that clones do positioned reads and cannot move each other's
        // cursor. If they could, two threads inflating different entries would
        // serve each other's bytes -- an asset rendered as the wrong asset,
        // with nothing reporting an error anywhere.
        let dir = scratch("apk-reader");
        let path = fake_apk(&dir, "base.apk", b"0123456789");
        let mut a = ApkReader::open(&path).unwrap();
        let mut b = a.clone();

        let mut first = [0u8; 4];
        a.read_exact(&mut first).unwrap();
        assert_eq!(&first, b"0123");

        // `b` has not moved, even though `a` has.
        let mut second = [0u8; 4];
        b.read_exact(&mut second).unwrap();
        assert_eq!(&second, b"0123");

        // And seeking from the end works, which is how the zip reader finds
        // the end-of-central-directory record in the first place.
        assert_eq!(b.seek(SeekFrom::End(-2)).unwrap(), 8);
        let mut tail = [0u8; 2];
        b.read_exact(&mut tail).unwrap();
        assert_eq!(&tail, b"89");
    }

    #[test]
    fn an_apk_reader_refuses_to_seek_before_the_start() {
        let dir = scratch("apk-reader-underflow");
        let path = fake_apk(&dir, "base.apk", b"abc");
        let mut r = ApkReader::open(&path).unwrap();
        assert!(r.seek(SeekFrom::End(-99)).is_err());
        // The failed seek must not have moved it -- a position quietly
        // clamped to zero would read the wrong entry rather than fail.
        assert_eq!(r.seek(SeekFrom::Current(0)).unwrap(), 0);
    }

    #[test]
    fn extraction_writes_the_assets_and_stamps_only_at_the_end() {
        let dir = scratch("extract-basic");
        let apk = fake_apk(&dir, "base.apk", b"apk-v1");
        let zip = apk_with(&[
            ("assets/content/a.txt", b"one"),
            ("assets/ssl/cacert.pem", b"pem"),
            // Not under `assets/`, so not ours to extract.
            ("lib/x86_64/libroblox.so", b"elf"),
        ]);
        let out = dir.join("assets");

        extract_archive_to(&out, &apk, archive_of(&zip)).unwrap();

        assert_eq!(std::fs::read(out.join("content/a.txt")).unwrap(), b"one");
        assert_eq!(std::fs::read(out.join("ssl/cacert.pem")).unwrap(), b"pem");
        assert!(!out.join("lib/x86_64/libroblox.so").exists());
        // The stamp is the completion marker, so it has to be there now.
        assert!(cordial_update::cache::is_current(&out, &apk));
        // And no temporary is left behind.
        let leftovers: Vec<_> = std::fs::read_dir(out.join("content"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("cordial-"))
            .collect();
        assert!(leftovers.is_empty(), "left a temporary behind: {leftovers:?}");
    }

    #[test]
    fn a_new_roblox_build_re_extracts_rather_than_serving_the_old_builds_assets() {
        // The defect this replaces: the test was `out.exists()` per file, so a
        // new APK left every asset from the old build exactly where it was and
        // the engine ran the new build against the old build's content.
        let dir = scratch("extract-stale");
        let out = dir.join("assets");

        let old_apk = fake_apk(&dir, "old.apk", b"apk-v1");
        extract_archive_to(&out, &old_apk, archive_of(&apk_with(&[("assets/x.txt", b"old")])))
            .unwrap();
        assert_eq!(std::fs::read(out.join("x.txt")).unwrap(), b"old");

        let new_apk = fake_apk(&dir, "new.apk", b"apk-v2-longer");
        assert!(!cordial_update::cache::is_current(&out, &new_apk));
        extract_archive_to(&out, &new_apk, archive_of(&apk_with(&[("assets/x.txt", b"new")])))
            .unwrap();
        assert_eq!(std::fs::read(out.join("x.txt")).unwrap(), b"new");
        assert!(cordial_update::cache::is_current(&out, &new_apk));
    }

    #[test]
    fn an_unchanged_apk_is_current_and_needs_no_second_extraction() {
        let dir = scratch("extract-unchanged");
        let out = dir.join("assets");
        let apk = fake_apk(&dir, "base.apk", b"apk-v1");
        extract_archive_to(&out, &apk, archive_of(&apk_with(&[("assets/x.txt", b"one")])))
            .unwrap();
        // `extract_to`'s early return keys on exactly this, and it is the one
        // thing standing between a launch and re-unpacking ninety megabytes.
        assert!(cordial_update::cache::is_current(&out, &apk));
    }

    #[test]
    fn an_interrupted_extraction_is_not_mistaken_for_a_finished_one() {
        // A partial tree with the right filenames in it -- what an extraction
        // killed part way leaves behind. Under the old per-file `exists()`
        // check every one of these counted as done and the missing remainder
        // was never noticed. The stamp is written last and only on success, so
        // a tree without one is not current whatever is in it.
        let dir = scratch("extract-partial");
        let out = dir.join("assets");
        let apk = fake_apk(&dir, "base.apk", b"apk-v1");
        write(&out, "content/a.txt", b"half");
        assert!(out.join("content/a.txt").exists());
        assert!(!cordial_update::cache::is_current(&out, &apk));

        extract_archive_to(
            &out,
            &apk,
            archive_of(&apk_with(&[("assets/content/a.txt", b"whole"), ("assets/b.txt", b"b")])),
        )
        .unwrap();
        // The half-written file is replaced rather than skipped.
        assert_eq!(std::fs::read(out.join("content/a.txt")).unwrap(), b"whole");
        assert_eq!(std::fs::read(out.join("b.txt")).unwrap(), b"b");
        assert!(cordial_update::cache::is_current(&out, &apk));
    }

    #[test]
    fn an_entry_that_escapes_the_destination_is_not_written() {
        // `enclosed_name` is the whole zip-slip defence and it is easy to
        // delete by accident while editing the loop around it.
        let dir = scratch("extract-slip");
        let out = dir.join("assets");
        let apk = fake_apk(&dir, "base.apk", b"apk-v1");
        extract_archive_to(
            &out,
            &apk,
            archive_of(&apk_with(&[
                ("assets/../../escaped.txt", b"no"),
                ("assets/kept.txt", b"yes"),
            ])),
        )
        .unwrap();
        assert!(out.join("kept.txt").exists());
        assert!(!dir.parent().unwrap().join("escaped.txt").exists());
        assert!(!dir.join("escaped.txt").exists());
    }

    fn layer(source: OverlaySource, root: &Path) -> OverlayLayer {
        OverlayLayer { source, root: root.to_path_buf() }
    }

    /// Point the user's root at an empty directory and clear the global state,
    /// so a test of the live stack cannot be decided by whatever the developer
    /// happens to have in `~/.config/cordial/overlay`.
    fn isolate(tag: &str) -> PathBuf {
        let empty = scratch(&format!("{tag}-user"));
        std::env::set_var("CORDIAL_OVERLAY", &empty);
        if let Ok(mut stack) = PLUGIN_OVERLAYS.lock() {
            stack.clear();
        }
        if let Ok(mut held) = REQUESTED.lock() {
            *held = None;
        }
        invalidate();
        empty
    }

    #[test]
    fn the_user_root_beats_a_plugin_root() {
        // The user always wins over anything they installed to do something
        // else — the same rule flags.rs already enforces for FastFlags, and
        // for the same reason: a plugin quietly overriding a choice the user
        // made on purpose would make "the user's own overlay" a polite
        // fiction rather than a real one.
        let dir = scratch("user-beats-plugin");
        let plugin_dir = dir.join("plugin");
        let user_dir = dir.join("user");
        write(&plugin_dir, "content/textures/wood.png", b"plugin-version");
        write(&user_dir, "content/textures/wood.png", b"user-version");

        let index = build_index(&[
            layer(OverlaySource::Plugin("themer".into()), &plugin_dir),
            layer(OverlaySource::User, &user_dir),
        ]);
        let hit = index.get("content/textures/wood.png").unwrap();
        assert_eq!(hit.source, OverlaySource::User);
        assert_eq!(std::fs::read(&hit.path).unwrap(), b"user-version");
    }

    #[test]
    fn the_last_registered_plugin_wins() {
        // Two plugins wanting the same asset is a real disagreement between
        // them, and resolving it by directory-iteration order would make the
        // outcome depend on something nobody chose. Registration order is at
        // least a fact — deterministic, and one a user can be told.
        let dir = scratch("last-plugin-wins");
        let a = dir.join("a");
        let b = dir.join("b");
        write(&a, "content/sounds/click.ogg", b"a");
        write(&b, "content/sounds/click.ogg", b"b");

        let index = build_index(&[
            layer(OverlaySource::Plugin("a".into()), &a),
            layer(OverlaySource::Plugin("b".into()), &b),
        ]);
        let hit = index.get("content/sounds/click.ogg").unwrap();
        assert_eq!(hit.source, OverlaySource::Plugin("b".into()));
        assert_eq!(std::fs::read(&hit.path).unwrap(), b"b");
    }

    #[test]
    fn the_loser_of_a_collision_is_named_rather_than_dropped() {
        // "Why did this file not change" is otherwise indistinguishable from
        // "the overlay is broken", and it is the question users ask most. The
        // losers only exist at merge time, so a map holding winners alone
        // cannot answer it afterwards at any price.
        let dir = scratch("shadow");
        let a = dir.join("retro-ui");
        let b = dir.join("my-fonts");
        write(&a, "content/fonts/SourceSansPro-Regular.ttf", b"retro");
        write(&b, "content/fonts/SourceSansPro-Regular.ttf", b"mine");

        let index = build_index(&[
            layer(OverlaySource::Plugin("retro-ui".into()), &a),
            layer(OverlaySource::User, &b),
        ]);
        let shadowing = index.shadowing();
        assert_eq!(shadowing.len(), 1);
        let (name, resolved) = shadowing[0];
        assert_eq!(name, "content/fonts/SourceSansPro-Regular.ttf");
        assert_eq!(resolved.source.describe(), "user");
        assert_eq!(
            resolved.shadowed.iter().map(OverlaySource::describe).collect::<Vec<_>>(),
            vec!["plugin:retro-ui".to_string()]
        );
    }

    #[test]
    fn a_name_nothing_provides_is_absent_from_the_index() {
        // Most assets are not overlaid. The common case has to behave as
        // "nothing here, keep looking" rather than "nothing here, fail the
        // lookup" — a miss in the map is exactly what lets Manager::read
        // carry on to the zip, and it costs the same probe as a hit, which is
        // why there is no separate negative cache.
        let dir = scratch("miss");
        write(&dir, "content/textures/other.png", b"unrelated");

        let index = build_index(&[layer(OverlaySource::Plugin("x".into()), &dir)]);
        assert!(index.get("content/textures/wood.png").is_none());
        assert!(index.get("content/textures/other.png").is_some());
    }

    #[test]
    fn a_symlink_leaving_the_root_never_enters_the_index() {
        // A name with no ".." in it can still escape if a path component is a
        // symlink. The containment check moved from per-lookup to per-file at
        // build time, so this is the test that it did not get lost on the way:
        // an escaping entry has to be absent from the map, not merely refused
        // later by something that remembers to ask.
        let dir = scratch("symlink");
        let outside = std::env::temp_dir().join("cordial-overlay-test-symlink-target");
        std::fs::write(&outside, b"outside").unwrap();
        write(&dir, "content/legit.png", b"inside");
        std::os::unix::fs::symlink(&outside, dir.join("content/escape.png")).unwrap();

        let index = build_index(&[layer(OverlaySource::Plugin("x".into()), &dir)]);
        assert!(index.get("content/escape.png").is_none(), "a symlink out of the root must not be indexed");
        assert!(index.get("content/legit.png").is_some(), "the defence is about leaving the root, not refusing everything");
    }

    #[test]
    fn a_root_that_does_not_exist_contributes_nothing_rather_than_failing() {
        // The overwhelmingly common case is a user with no overlay directory
        // at all. It has to be free and silent, not an error path.
        let index = build_index(&[layer(OverlaySource::User, Path::new("/nonexistent/cordial-overlay"))]);
        assert!(index.is_empty());
    }

    #[test]
    fn removing_a_root_restores_the_original_with_no_cleanup_step() {
        // This is the whole point of an overlay that only ever reads:
        // uninstalling a plugin is "stop consulting its directory", full
        // stop. There is nothing on disk to undo, because the asset the
        // overlay was standing in for was never touched.
        let _guard = GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
        isolate("removal");
        let dir = scratch("removal-plugin");
        write(&dir, "content/fonts/custom.ttf", b"overlay-font");

        register_plugin_root("overlay-removal-test", dir.clone());
        assert_eq!(
            resolve_overlay("content/fonts/custom.ttf").map(|(s, _)| s),
            Some(OverlaySource::Plugin("overlay-removal-test".into()))
        );

        unregister_plugin_root("overlay-removal-test");
        // Nothing else provides it, so this is exactly what a real lookup
        // would see: no overlay hit, fall through to the APK.
        assert!(resolve_overlay("content/fonts/custom.ttf").is_none());
    }

    #[test]
    fn registering_a_root_invalidates_an_index_already_built() {
        // The index is the thing that makes a lookup one probe, and a stale
        // one is a plugin that installed and did nothing. Registration has to
        // drop it, or hot reload and plugin startup both silently fail in the
        // same way.
        let _guard = GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
        isolate("invalidate");
        assert!(index().is_empty(), "isolated: nothing registered yet");

        let dir = scratch("invalidate-plugin");
        write(&dir, "content/textures/new.png", b"new");
        register_plugin_root("late", dir);
        assert!(index().get("content/textures/new.png").is_some(), "the index must have been rebuilt");

        unregister_plugin_root("late");
    }

    #[test]
    fn a_reload_says_how_many_names_it_could_not_affect() {
        // Reporting "reloaded" while the old texture is still on screen is the
        // stub-that-lies failure AGENTS.md forbids. Nothing has been served
        // here, so the honest answer is zero — and the field has to exist and
        // be reported for the case where it is not.
        let _guard = GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
        isolate("reload");
        let dir = scratch("reload-plugin");
        write(&dir, "content/textures/a.png", b"a");
        write(&dir, "content/textures/b.png", b"b");
        register_plugin_root("reloadable", dir);

        let report = reload();
        assert_eq!(report.files, 2);
        assert_eq!(report.layers, 2, "the plugin root and the user root");
        assert_eq!(report.already_cached, 0);

        unregister_plugin_root("reloadable");
    }

    #[test]
    fn watching_is_off_unless_asked_for() {
        // Watching a directory tree costs wakeups, and a texture pack nobody
        // is editing must not cost one. Reload is an explicit call by default.
        let _guard = GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("CORDIAL_OVERLAY_WATCH");
        assert!(!watch_enabled());
        std::env::set_var("CORDIAL_OVERLAY_WATCH", "1");
        assert!(watch_enabled());
        std::env::set_var("CORDIAL_OVERLAY_WATCH", "0");
        assert!(!watch_enabled(), "an explicit 0 must mean off, not merely 'set'");
        std::env::remove_var("CORDIAL_OVERLAY_WATCH");
    }

    #[test]
    fn a_write_is_never_redirected_to_an_overlay() {
        // Decided in ADR-021 rather than discovered from a corrupted cache.
        // The overlay is read-only by definition: handing a writable fd to a
        // plugin's file would make it somewhere the engine can scribble, which
        // is neither non-destructive nor anything the plugin's author agreed
        // to. Reads resolve to the overlay; writes go to the original.
        let _guard = GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
        isolate("write");
        let root = scratch("write-assets");
        std::fs::create_dir_all(root.join("content")).unwrap();
        let overlay = scratch("write-overlay");
        write(&overlay, "content/cache.bin", b"overlay");
        register_plugin_root("writer", overlay);
        set_asset_root(&root);

        let target = root.join("content/cache.bin").to_string_lossy().into_owned();
        assert!(resolve_asset_path(&target, false).is_some(), "a read must find the overlay");
        assert!(resolve_asset_path(&target, true).is_none(), "a write must fall through to the real file");

        unregister_plugin_root("writer");
    }

    #[test]
    fn write_intent_is_read_off_the_open_flags() {
        // O_RDONLY is 0, so a naive `flags != 0` test would call every
        // O_CLOEXEC read a write. The creating and truncating flags are
        // checked beside the access mode so an O_RDONLY|O_CREAT cannot slip
        // past as a read.
        const O_RDONLY: i32 = 0;
        const O_WRONLY: i32 = 0o1;
        const O_RDWR: i32 = 0o2;
        const O_CREAT: i32 = 0o100;
        const O_CLOEXEC: i32 = 0o2000000;
        assert!(!is_write_intent(O_RDONLY));
        assert!(!is_write_intent(O_RDONLY | O_CLOEXEC));
        assert!(is_write_intent(O_WRONLY));
        assert!(is_write_intent(O_RDWR));
        assert!(is_write_intent(O_RDONLY | O_CREAT));
    }

    #[test]
    fn a_path_outside_the_asset_root_is_left_alone() {
        // The overlay must never become a general filesystem redirect — the
        // same line ADR-007 draws between an effect and a channel. And the
        // failure has to be closed: the first thing the engine resolves by
        // real path is ssl/cacert.pem, and getting that wrong produces a TLS
        // failure three layers from anything mentioning certificates.
        let _guard = GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
        isolate("outside");
        let root = scratch("outside-assets");
        let overlay = scratch("outside-overlay");
        write(&overlay, "content/textures/wood.png", b"overlay");
        register_plugin_root("outsider", overlay);
        set_asset_root(&root);

        assert!(resolve_asset_path("/etc/passwd", false).is_none());
        assert!(resolve_asset_path("content/textures/wood.png", false).is_none(), "a relative path is not under the root");
        assert!(
            resolve_asset_path(&root.join("content/textures/wood.png").to_string_lossy(), false).is_some(),
            "a path genuinely under the asset root still resolves"
        );

        unregister_plugin_root("outsider");
    }

    #[test]
    fn the_recorder_keeps_which_layer_answered_each_name() {
        // One cold launch plus one game join is the ground-truth list of what
        // this build actually reads, and it is what both orphan signals are
        // computed against. A set rather than a log, so a name already seen
        // costs a lookup and no allocation.
        let _guard = GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
        isolate("recorder");
        record("content/textures/wood.png", Served::Overlay(OverlaySource::Plugin("retro-ui".into())));
        record("content/sounds/oof.ogg", Served::Apk);
        record("content/textures/absent.png", Served::Missing);

        let asked = requested();
        assert_eq!(asked.len(), 3);
        assert_eq!(asked["content/textures/wood.png"].describe(), "plugin:retro-ui");
        assert_eq!(asked["content/sounds/oof.ogg"].describe(), "apk");
        assert_eq!(asked["content/textures/absent.png"].describe(), "missing");

        let out = scratch("recorder-out").join("asset-trace.log");
        assert_eq!(write_trace(&out).unwrap(), 3);
        let text = std::fs::read_to_string(&out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        // Sorted on the way out, so `sort -u` on it is idempotent and two
        // runs diff without preprocessing.
        assert_eq!(lines[0], "content/sounds/oof.ogg\tapk");
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn a_stale_overlay_file_is_the_one_that_matches_nothing_in_the_build() {
        // The signal that catches the most famous Bloxstrap mod there is:
        // it ships content/sounds/ouch.ogg and this build reads
        // content/sounds/oof.ogg, so the mod is installed, enabled, correct
        // in every other respect, and silently does nothing. Checked against
        // the real archive on this host — the miss is real, not illustrative.
        let _guard = GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
        isolate("stale");
        let overlay = scratch("stale-overlay");
        write(&overlay, "content/sounds/ouch.ogg", b"classic oof");
        write(&overlay, "content/sounds/oof.ogg", b"the one that lands");
        register_plugin_root("retro-ui", overlay);

        let apk: BTreeSet<String> = ["content/sounds/oof.ogg".to_string()].into_iter().collect();
        let orphans = stale(&apk);
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].name, "content/sounds/ouch.ogg");
        assert_eq!(orphans[0].source, OverlaySource::Plugin("retro-ui".into()));

        let report = stale_report(&apk, "2.7xx");
        assert_eq!(report, vec!["plugin:retro-ui: 1 file no longer matches anything in client 2.7xx".to_string()]);

        unregister_plugin_root("retro-ui");
    }

    #[test]
    fn unrequested_is_a_different_claim_from_stale() {
        // Stale means the file can never apply. Unrequested means it did not
        // apply today, possibly only because the run was short. Merging them
        // into one count reports a certainty and a maybe as the same fact,
        // which is the thing this pair exists to keep apart.
        let _guard = GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
        isolate("unrequested");
        let overlay = scratch("unrequested-overlay");
        write(&overlay, "content/textures/asked.png", b"a");
        write(&overlay, "content/textures/never.png", b"b");
        register_plugin_root("pack", overlay);

        let apk: BTreeSet<String> =
            ["content/textures/asked.png".to_string(), "content/textures/never.png".to_string()]
                .into_iter()
                .collect();
        assert!(stale(&apk).is_empty(), "both names exist in the build, so neither is stale");

        record("content/textures/asked.png", Served::Overlay(OverlaySource::Plugin("pack".into())));
        let quiet = unrequested();
        assert_eq!(quiet.len(), 1);
        assert_eq!(quiet[0].name, "content/textures/never.png");

        unregister_plugin_root("pack");
    }

    #[test]
    fn the_signature_notices_a_file_appearing() {
        // What the watcher polls. If this cannot tell one state from another,
        // `CORDIAL_OVERLAY_WATCH=1` is a switch that silently does nothing --
        // which is exactly the stub-that-lies shape AGENTS.md forbids, moved
        // into a thread where it is harder to notice.
        let _guard = GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
        isolate("signature");
        let dir = scratch("signature-plugin");
        write(&dir, "content/textures/a.png", b"a");
        register_plugin_root("watched", dir.clone());

        let before = roots_signature();
        write(&dir, "content/textures/b.png", b"b");
        let after = roots_signature();
        assert_ne!(before, after, "a new file must change the signature");
        assert_eq!(after.0, before.0 + 1);

        unregister_plugin_root("watched");
    }

    #[test]
    fn a_tick_reloads_only_when_something_changed() {
        // The whole watcher, minus the sleep. A tick that reloaded every time
        // would rebuild the index twice a second for a directory nobody is
        // editing, which is the cost this codebase has a rule about; one that
        // never reloaded would make the switch do nothing.
        let _guard = GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
        isolate("tick");
        let dir = scratch("tick-plugin");
        write(&dir, "content/textures/a.png", b"a");
        register_plugin_root("ticker", dir.clone());

        let mut last = roots_signature();
        assert!(watch_tick(&mut last).is_none(), "nothing changed, so nothing to do");

        write(&dir, "content/textures/b.png", b"b");
        let report = watch_tick(&mut last).expect("a new file must trigger a reload");
        assert_eq!(report.files, 2);
        assert!(watch_tick(&mut last).is_none(), "the signature must be carried forward");

        unregister_plugin_root("ticker");
    }

    #[test]
    fn no_watcher_is_started_unless_asked_for() {
        let _guard = GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("CORDIAL_OVERLAY_WATCH");
        assert!(!start_watcher(), "a texture pack nobody is editing must not cost a wakeup");
    }

    #[test]
    fn a_version_shaped_token_is_told_from_a_library_version() {
        // The scan runs over a string pool that also holds every AndroidX
        // library's version, so a loose pattern reports one of those as the
        // client's. "7 files no longer match anything in client 7.1.1" would
        // be confidently wrong, which is worse than saying nothing.
        assert!(is_client_version("2.734.917"));
        assert!(!is_client_version("7.1.1"), "a library version must not be mistaken for the client's");
        assert!(!is_client_version("1.2"));
        assert!(!is_client_version("2.734.917-beta"));
        assert!(!is_client_version(""));
    }

    #[test]
    fn the_shadow_report_names_the_winner_first() {
        // The line a user reads. It has to say who won, because the whole
        // point is answering "why is my file not the one being used".
        let _guard = GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
        let user = isolate("shadow-report");
        write(&user, "content/fonts/x.ttf", b"mine");
        let plugin = scratch("shadow-report-plugin");
        write(&plugin, "content/fonts/x.ttf", b"theirs");
        register_plugin_root("retro-ui", plugin);
        invalidate();

        let report = shadow_report();
        assert_eq!(report, vec!["user wins over plugin:retro-ui   content/fonts/x.ttf".to_string()]);

        unregister_plugin_root("retro-ui");
    }
}
