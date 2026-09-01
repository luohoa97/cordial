//! Layered FastFlag overrides, with provenance.
//!
//! Flags come from more than one place: Roblox's own settings document, the
//! user, and — once plugins exist — any plugin that needs a flag set to do its
//! job. Merging those into one file would be the obvious thing and it is wrong
//! in three specific ways:
//!
//! * a plugin could silently overwrite a value the user chose deliberately;
//! * uninstalling a plugin would leave its flags behind, because nothing records
//!   which line belonged to whom;
//! * "why is this flag set to that?" would have no answer.
//!
//! So each source owns its own file and they are resolved in a fixed order,
//! recording where every effective value came from.
//!
//! ```text
//! <profile>/flags.json                                user      (wins)
//! ~/.local/share/cordial/plugins/<id>/flags.json      plugin
//! the client-settings document from Roblox            base
//! ```
//!
//! **The user always wins.** An explicit setting is the one thing that must not
//! be overridable by software the user installed to do something else.
//!
//! **The user's file is per profile** (ADR-013). It used to be one file at
//! `~/.config/cordial/flags.json` for the whole machine, which meant a flag set
//! while debugging something on a test account was silently still set on the
//! account someone actually plays — and flags are exactly the setting people
//! change temporarily and forget. `migrate_legacy_user_file` moves an existing
//! one in rather than leaving it to be ignored, because a launch that quietly
//! stopped honouring overrides is indistinguishable from the overrides never
//! having worked.
//!
//! A plugin's own `flags.json` stays beside its installed code, which is global
//! — a plugin is installed once. That is a real asymmetry and it is recorded in
//! ADR-013's open question, because an installed plugin currently contributes
//! its flags in every profile whether or not it was granted anything there.
//!
//! **Plugin-against-plugin conflicts are reported, not resolved.** Two plugins
//! wanting the same flag set differently is a real disagreement, and picking one
//! by filesystem order would hide it. Both are named and the first-loaded wins,
//! so the behaviour is at least deterministic and visible.
//!
//! One property matters to anything built on this, and it is not obvious:
//! `FFlag`, `FInt` and `FString` are read once during engine startup, so a
//! plugin can only influence them if its flags are resolved *before the process
//! starts*. Only the `DFFlag`/`DFInt`/`DFString` family is re-read while the
//! client runs. A plugin loaded on demand, part-way through a session, cannot
//! change a startup flag no matter what it writes.
//!
//! **Being re-read is a cost, not a capability, and this paragraph used to read
//! as though it were the other way round.** The engine fetches Roblox's settings
//! document itself about two seconds in and applies it over the top, so a
//! `DF*` override of a key Roblox also sets is reverted to Roblox's value while
//! the client is still starting; see `client_settings::apply_overrides` for the
//! two-directional control that establishes it. The durable family is the one
//! read once. An override in either family is still worth writing — this layer
//! is how it reaches the engine at all — but a `DF*` one governs the first
//! couple of seconds and a measurement resting on it has to fall inside them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Where an effective flag value came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// The user's own `flags.json`.
    User,
    /// A plugin, by its directory name.
    Plugin(String),
    /// Cordial's own defaults, below every other layer.
    Builtin,
    /// The engine worker-pool flags the chosen [`Performance`] mode asks for.
    ///
    /// A source of its own rather than folded into [`Source::Builtin`],
    /// because the two answer different questions and the report is read to
    /// find out why a flag has the value it has. `Builtin` means "Cordial
    /// always sets this"; this means "you chose a mode in Settings and it set
    /// this". Sitting immediately above `Builtin` and below every plugin, it
    /// is a default a plugin or the user may still overrule -- which is right
    /// for a mode whose tables nothing on this project's hardware has
    /// measured.
    Performance,
}

impl Source {
    pub fn describe(&self) -> String {
        match self {
            Source::User => "user".into(),
            Source::Plugin(id) => format!("plugin:{id}"),
            Source::Builtin => "built-in".into(),
            Source::Performance => "performance mode".into(),
        }
    }
}

/// One resolved override: the value that will be applied, and who asked for it.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub value: String,
    pub source: Source,
    /// Other sources that wanted this flag and lost, in the order they were read.
    pub overridden: Vec<(Source, String)>,
}

/// A set of overrides read from one file.
pub struct Layer {
    pub source: Source,
    pub values: BTreeMap<String, String>,
}

/// Read a flat JSON object of flag -> value.
///
/// Roblox stores every setting as a string, so booleans and numbers are
/// converted rather than refused — a config file should not require knowing
/// that. Returns `None` when the file is absent, and reports and skips when it
/// is present but malformed: a typo should not stop the client from starting.
///
/// **This conversion is not the reason an `FLog`/`DFLog` override can appear
/// to do nothing, or to silence a channel — established by experiment**
/// (`docs/analysis/flag-init.md` §22, corrected in the addendum after it).
/// Roblox's own settings document is entirely strings, and it is
/// *heterogeneous*: some logging channels are declared as a bare verbosity
/// number (`FLogNetwork = "7"`), others as a severity name with an optional
/// sub-level (`FLogAudio = "Info"`, `DFLogWebSocketTraceError = "Warning,6"`,
/// both seen directly in the cached document). Which shape a given channel
/// wants is a property of its own C++ declaration, invisible from here, and
/// this function does not try to guess it — Roblox's own document is not
/// internally consistent about it either, so guessing wrong here would be no
/// worse than guessing wrong there.
///
/// The failure mode when the wrong shape is sent looks exactly like this
/// function doing something wrong. §22 measured `FLogNativeDM` at `"1"`,
/// `"9"` and `"100"` — three bare numbers — and every one silenced a channel
/// that logs 12–30 lines when left unset, which read as "naming a channel can
/// quiet it". That does not survive a repeat with a severity name instead of
/// a number: `{"FLogNativeDM": "Verbose"}` left the channel at or above its
/// unset count (29 unset, 30 overridden, repeated), and the identical
/// override raised `FLogAppShellReporter` from 0 to 14–16 lines exactly as a
/// bare `"7"` or `"9"` already did elsewhere. So the conversion below was
/// never the fault: it passes `"7"` and `"Verbose"` through with equal
/// fidelity, which is all it should do. The engine's own parser, handed a
/// value of the wrong shape for a particular channel, fails safe to silent
/// rather than falling back to that channel's compiled default — and a
/// silenced channel is indistinguishable from a broken override unless the
/// shape is the first thing checked.
pub fn read_layer(path: &Path, source: Source) -> Option<Layer> {
    let text = std::fs::read_to_string(path).ok()?;
    let parsed: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            println!("  flags: {} is not valid JSON ({e}); ignoring", path.display());
            return None;
        }
    };
    let Some(obj) = parsed.as_object() else {
        println!("  flags: {} is not a JSON object; ignoring", path.display());
        return None;
    };
    let values = obj
        .iter()
        .map(|(k, v)| {
            let s = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            (k.clone(), s)
        })
        .collect();
    Some(Layer { source, values })
}

/// Resolve layers into one set of effective values.
///
/// `layers` is in precedence order, lowest first, so the user's layer goes last.
/// Everything a losing layer wanted is kept on the winner rather than discarded,
/// which is what makes the result explainable afterwards.
pub fn resolve(layers: Vec<Layer>) -> BTreeMap<String, Resolved> {
    let mut out: BTreeMap<String, Resolved> = BTreeMap::new();
    for layer in layers {
        for (key, value) in layer.values {
            match out.get_mut(&key) {
                Some(existing) => {
                    let previous = std::mem::replace(&mut existing.value, value);
                    let previous_source =
                        std::mem::replace(&mut existing.source, layer.source.clone());
                    existing.overridden.push((previous_source, previous));
                }
                None => {
                    out.insert(
                        key,
                        Resolved { value, source: layer.source.clone(), overridden: Vec::new() },
                    );
                }
            }
        }
    }
    out
}

/// The user's overrides file, inside `profile_dir`.
///
/// `CORDIAL_FLAGS` still overrides it outright, which is how a one-off
/// experiment points at a file of its own. That override is global by nature —
/// it makes one file serve every profile — so it is a development switch, not a
/// supported arrangement.
pub fn user_path_in(profile_dir: &Path) -> PathBuf {
    // Delegated so the settings window, which cannot depend on this crate, can
    // name the identical file. See `cordial_plugins::flag_document::path_in`.
    cordial_plugins::flag_document::path_in(profile_dir)
}

/// The user's overrides file for the profile this instance is running.
pub fn user_path() -> PathBuf {
    user_path_in(&crate::profile::active())
}

/// Where the user's overrides lived before they were per profile.
pub fn legacy_user_path() -> PathBuf {
    config_dir().join("cordial/flags.json")
}

/// Move a pre-existing global overrides file into `profile_dir`, once.
///
/// Same guard and the same reasoning as `profile::migrate_legacy_layout` and
/// `cordial_plugins::grants::migrate_legacy_into`: only when the old file
/// exists and the new one does not, so it can never overwrite overrides already
/// made in this profile. Moved rather than copied, and into whichever profile
/// first goes looking, because there is no record of which profile a global
/// file was meant for — it was meant for all of them, which is the thing being
/// fixed. In practice that is `default`, where ADR-012's migration lands
/// existing storage.
pub fn migrate_legacy_user_file(profile_dir: &Path) -> Option<PathBuf> {
    let legacy = legacy_user_path();
    let target = user_path_in(profile_dir);
    if !legacy.is_file() || target.exists() {
        return None;
    }
    std::fs::create_dir_all(profile_dir).ok()?;
    match std::fs::rename(&legacy, &target) {
        Ok(()) => {
            println!(
                "  flags: moved {} to {} (ADR-013)",
                legacy.display(),
                target.display()
            );
            Some(target)
        }
        // A cross-device rename is the plausible failure. Leaving the old file
        // alone and saying so beats a half-written overrides document, which
        // would be reported as invalid JSON and ignored entirely.
        Err(e) => {
            println!(
                "  flags: could not move {} to {} ({e}); the old location is untouched",
                legacy.display(),
                target.display()
            );
            None
        }
    }
}

/// The **writable** plugin directory: where a plugin's own overrides live, one
/// subdirectory per plugin id. A plugin owns its file and nothing else writes
/// to it, so removing the plugin removes its flags.
///
/// This is the user's directory specifically, and the write path uses it
/// directly rather than searching — [`plugin_dirs`] is for *discovery*, and its
/// first entry is read-only.
pub fn plugin_dir() -> PathBuf {
    std::env::var_os("CORDIAL_PLUGIN_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| data_dir().join("cordial/plugins"))
}

/// Where first-party plugins ship, read-only, installed alongside the binary.
///
/// Delegates to [`cordial_plugins::manifest::system_plugin_root`], which is now
/// the single definition: the settings window has to list built-in plugins
/// beside user ones, and it reaches `cordial-plugins` rather than this crate.
/// This wrapper stays so the two-tier search path below still reads as one
/// idea in one place.
pub fn system_plugin_dir() -> PathBuf {
    cordial_plugins::manifest::system_plugin_root()
}

/// Every directory plugins are discovered from, **system first**.
///
/// The same two-tier arrangement GNOME Shell shows as "System Extensions" and
/// "User-Installed Extensions", and Flatpak as system and user installations.
///
/// **A user plugin may not shadow a first-party one, and this is deliberate.**
/// The usual XDG convention lets the user directory win, but a first-party
/// plugin ships a trusted opinion — the built-in web-view interceptor decides
/// that a Studio link means the software centre — and a same-id directory in a
/// writable location silently replacing that is an impersonation route, not a
/// customisation. So on a collision the system copy is used and the conflict is
/// **reported by name** rather than resolved quietly. See [`collect`].
///
/// Disabling a first-party plugin is a different thing and is built:
/// `cordial_plugins::enablement` records it per profile, the settings window
/// has a switch for every plugin in both tiers plus a master one, and
/// [`collect`] below skips a disabled plugin's layer. Refusing to shadow was
/// never a substitute for that, and no longer has to be.
pub fn plugin_dirs() -> Vec<PathBuf> {
    vec![system_plugin_dir(), plugin_dir()]
}

/// How large one plugin's own `flags.json` may grow.
///
/// Nothing here validates the *keys* a plugin sends — only Roblox's own C++
/// knows which FastFlag names are real, and an unrecognised one simply
/// resolves to nothing, harmlessly. The size cap exists for the reason
/// `settings.rs`'s does: an ordinary bug that keeps appending values must not
/// be able to fill a plugin's directory silently just because nothing else
/// was watching.
const MAX_PLUGIN_FLAGS_BYTES: usize = 256 * 1024;

/// Replace plugin `id`'s own `flags.json` with `values` — the effect behind
/// the `flags.write` capability (`plugin_host.rs`'s `dispatch`).
///
/// A whole-document replace, not a merge, for the same reason
/// `settings::Store::write` is: a plugin is the only writer of its own file,
/// so it always knows the complete set it means to leave in place, and a
/// merge would give it no way to withdraw a flag it has stopped wanting
/// overridden. Written to a sibling file and renamed in — the same
/// atomic-write shape every other per-plugin document in this project
/// uses — so a process killed mid-write leaves the previous, valid document
/// rather than one `read_layer` has to report and skip.
///
/// Takes effect at the **next launch only**: `FFlag`/`FInt`/`FString` are
/// read once during `nativeInitClientSettings`, and this file is one layer
/// `collect` reads before the engine process starts (see the module note) —
/// there is no live effect here to have, which is the whole reason
/// `flags.write` and `flags.write.dynamic` are two capabilities rather than
/// one (ADR-005).
pub fn write_plugin_layer(id: &str, values: &BTreeMap<String, String>) -> Result<(), String> {
    if !cordial_plugins::manifest::is_valid_id(id) {
        return Err(format!("{id:?} is not a usable plugin id"));
    }
    let text = serde_json::to_string_pretty(values).map_err(|e| e.to_string())?;
    if text.len() > MAX_PLUGIN_FLAGS_BYTES {
        return Err(format!(
            "{} bytes of flags is more than the {MAX_PLUGIN_FLAGS_BYTES} byte limit",
            text.len()
        ));
    }
    let dir = plugin_dir().join(id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join("flags.json");
    let tmp = path.with_extension("json.new");
    std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Parse the text of a user flags document, the way [`read_layer`] would.
///
/// Delegates to [`cordial_plugins::flag_document::parse`], which is where the
/// format lives so that the settings window can use the identical rule --
/// `cordial-runtime` depends on `cordial-shell`, so the window cannot depend
/// back on this crate. That module's header has the argument in full; the
/// short version is that two implementations of "is this document valid" is
/// how a window comes to say "Saved 40 flags" about a file the client then
/// ignores.
pub fn parse_user_text(text: &str) -> Result<BTreeMap<String, String>, String> {
    cordial_plugins::flag_document::parse(text)
}

/// Replace the user's own `flags.json` for `profile_dir`.
pub fn write_user_layer(profile_dir: &Path, values: &BTreeMap<String, String>) -> Result<(), String> {
    cordial_plugins::flag_document::write(&user_path_in(profile_dir), values)
}

fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(std::env::temp_dir)
}

fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(std::env::temp_dir)
}

/// Flags Cordial sets for itself, at the bottom of the stack.
///
/// **Empty, and the reason it is empty is worth keeping.**
///
/// It briefly carried `FStringGraphicsTextureManager2DenyPattern2 = ".*"`, taken
/// from a mocktail commit titled "Another fix for low quality texture" and
/// shipped here as a default on the strength of that title plus the flag's
/// absence from Roblox's own settings document. It was marked `INFERRED`,
/// because what was established was the flag's absence and its effect on
/// mocktail — never that it improved anything here.
///
/// Denying every pattern takes the engine off TextureManager 2 and onto the
/// legacy path, which a real session confirmed: the log prints
/// `[FLog::Graphics] Using TM1`. What that cost, reported from play, was a lobby
/// that refused to render and loading that was visibly slow. TM2 is the modern
/// manager and streams textures; the legacy path does not, so a large place pays
/// twice.
///
/// **And the blurring it was meant to fix has never been seen here.** Not once,
/// by anyone, on Cordial. The entire case for the flag was another project's
/// commit title. Whatever they were fixing may be specific to their stack, or
/// may have been something else with a texture-shaped name; either way this was
/// a remedy shipped for a symptom nobody in this project had reported.
///
/// One further observation, and it is why this is not filed as "TM1 is bad":
/// fullscreening the window made the lobby render correctly *with the deny in
/// place*. That points at the viewport rather than the texture manager, so the
/// render failure and the manager may not be the same fact at all. Untangling
/// that needs a side-by-side nobody has done.
///
/// The mistake was not adopting the flag. It was making an `INFERRED` change a
/// **default**. An inference belongs behind a switch somebody chooses, where
/// being wrong costs that person an experiment; as a default it costs everybody
/// a worse client and nobody knows why. Anything added here in future should be
/// something measured on this project's own hardware, not something read off
/// another project's commit title.
///
/// The flag itself still works through the ordinary layers — a user or plugin
/// can set it in `flags.json` — so nothing is lost except the default.
///
/// ## The one entry, and why it is Cordial's rather than a plugin's
///
/// `FFlagUserLaunchedWithBloxstrap` is how a game finds out that the launcher
/// it is running under implements BloxstrapRPC. An experience reads it with
/// `UserSettings():IsUserFeatureEnabled("UserLaunchedWithBloxstrap")` and
/// decides whether to bother printing presence at all, so without it a game
/// that gates on it stays silent and Cordial's whole BloxstrapRPC reader has
/// nothing to read.
///
/// **It is not an engine flag.** `LaunchedWithBloxstrap` does not appear
/// anywhere in `libroblox.so`; it is a name the launcher injects and the game
/// asks about. That it can be injected at all is established rather than
/// assumed — `client_settings`'s own call table records
/// `{"applicationSettings":{"FFlagNotARealFlag":"True"}}` returning `0`, so
/// the engine takes names it does not define.
///
/// **This belongs to Cordial and not to `discord-presence`.** The claim is
/// about the launcher, and it is Cordial's runtime that makes it true:
/// `bloxstrap_rpc` parses the protocol and `game_log` publishes
/// `cordial/game.presence` to whatever is listening, with no presence plugin
/// involved. Putting it in a plugin would mean a capability of the runtime
/// switching off when an unrelated plugin was disabled, two presence plugins
/// each setting it, and `discord-presence` needing `flags.write` for something
/// outside what it does -- ADR-007 brokers *effects*, and "tell games this
/// launcher speaks a protocol" is not a presence effect. `hide-gui` sets a
/// flag because that flag is its entire feature; this one is not.
///
/// **The name is a small lie and worth saying so.** Cordial is not Bloxstrap.
/// The flag is a de-facto protocol handshake that happens to be named after
/// the launcher that invented it, in the way a User-Agent claims Mozilla
/// compatibility, and no other name exists to send. The trade is that a game
/// can now tell it is running under a third-party launcher; that is a
/// fingerprinting signal and the reason this is worth a sentence rather than
/// a silent default. It is the lowest-precedence layer, so
/// `"FFlagUserLaunchedWithBloxstrap": "False"` in `flags.json` turns it off.
///
/// **INFERRED that it takes effect.** What is established is that the flag is
/// not the engine's, that the engine accepts unknown names, and that games ask
/// for it. That a game's `IsUserFeatureEnabled` then answers true has not been
/// seen here. Bloxstrap's own RPC test place reports it on a board and is the
/// instrument; until somebody stands in it and reads YES, this entry is a
/// reasoned guess and the paragraph above about measured-not-inferred applies
/// to it as much as to anything else.
const BUILTIN: &[(&str, &str)] = &[("FFlagUserLaunchedWithBloxstrap", "True")];

/// How hard to push the engine's own worker pools, as a choice rather than a
/// default.
///
/// Roblox exposes several thread-count and pipeline flags that mocktail's
/// `src/runtime/performance_policy.cc` scales to the machine (Apache-2.0; the
/// policy shape is adapted here, not its code). They are offered as modes for
/// the reason [`BUILTIN`] gives at length: an inference belongs behind a switch
/// somebody chooses, where being wrong costs that person an experiment, rather
/// than as a default where it costs everybody a worse client and nobody knows
/// why.
///
/// **None of these are measured on this project's hardware yet.** Two of them --
/// `FIntTaskSchedulerAutoThreadLimit` and `FIntTaskSchedulerThreadMin` -- were
/// tested against CPU on 2026-08-21 and moved it not at all, but CPU is not what
/// they are for and the frame rate was never measured. Until somebody runs the
/// comparison, [`Performance::Balanced`] sets nothing and is the default, so the
/// shipped behaviour is exactly what it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Performance {
    /// Sets nothing. What Cordial has always done.
    #[default]
    Balanced,
    /// More worker threads and parallel prerender, at the cost of using more of
    /// the machine. Aimed at a desktop with cores to spare.
    Throughput,
    /// Fewer threads and a smaller physics batch, for a machine where latency
    /// matters more than throughput or where cores are scarce.
    Latency,
}

/// The flags a mode asks for, given how many *physical* cores the machine has.
///
/// Physical rather than logical, and that distinction is not pedantry: on a
/// hybrid part, sizing a render worker pool by thread count spreads work onto
/// cores that are slower per thread, which is the opposite of the intent.
///
/// **Corrected.** This used to describe the machine it was written on as "six
/// performance cores and eight efficiency cores reporting sixteen threads".
/// That part is an i7-13620H and it is 6P+**4**E: twelve threads from six
/// hyperthreaded P-cores plus four unshared E-cores, so **ten** physical cores
/// and sixteen threads. Established by walking the same sysfs topology
/// [`physical_cores`] itself reads -- `cpu0..cpu11` give core_ids 0,4,8,12,16,20
/// twice each and `cpu12..cpu15` give 24,25,26,27 once each, ten distinct
/// (package, core) pairs -- and confirmed against `lscpu`, which reports
/// `Core(s) per socket: 10` and `CPU(s): 16`.
///
/// The function was always right; only this comment was wrong, and it was wrong
/// by four cores in the direction that matters, because every mode below sizes
/// its worker pools off this number. Anyone reasoning about those tables from
/// the comment rather than from the machine was reasoning from 14.
///
/// Pure, so the table can be tested without a machine to run it on.
pub fn performance_flags(mode: Performance, physical_cores: usize) -> Vec<(String, String)> {
    let workers = physical_cores.max(1);
    let owned = |k: &str, v: String| (k.to_string(), v);
    match mode {
        Performance::Balanced => Vec::new(),
        // The async minimum is capped at three in mocktail's policy; kept,
        // because a floor that rises with core count would put a large machine's
        // async pool above what the scheduler is asked to allow.
        Performance::Throughput => vec![
            owned("FIntTaskSchedulerThreadMin", "0".into()),
            owned("FIntTaskSchedulerAsyncTasksMinimumThreadCount", workers.min(3).to_string()),
            owned("FIntTaskSchedulerAutoThreadLimit", workers.to_string()),
            owned("FIntSmoothClusterTaskQueueMaxParallelTasks", workers.to_string()),
            owned("FIntOcclusionWorkerThreadCount", workers.div_ceil(2).max(1).to_string()),
            owned("FFlagMovePrerenderV2", "True".into()),
            owned("FFlagGcInParallelWithRenderPrepare3", "True".into()),
            owned("DFIntSimMidPhaseContactPipelineBatchSize", "128".into()),
        ],
        Performance::Latency => vec![
            owned("DFIntSimMidPhaseContactPipelineBatchSize", "128".into()),
            owned("FIntTaskSchedulerThreadMin", "0".into()),
        ],
    }
}

/// The environment variable the shell sets to choose a [`Performance`] mode.
///
/// Named and passed the same way [`DEVICE_PROFILE_ENV`] and `CORDIAL_GRAPHICS`
/// are, and for the same reason all three are environment rather than a file:
/// `FInt`/`FFlag` overrides are read once during engine startup, so the mode
/// has to be settled before the process starts. `crates/cordial-shell`'s
/// `launch.rs` sets it, and omits it entirely for the default -- an absent
/// variable is what leaves a plugin room to have an opinion, which is the
/// argument that block already makes about `CORDIAL_GRAPHICS`.
pub const PERFORMANCE_ENV: &str = "CORDIAL_PERFORMANCE";

impl Performance {
    pub fn parse(text: &str) -> Option<Performance> {
        match text.trim().to_ascii_lowercase().as_str() {
            "" | "balanced" => Some(Performance::Balanced),
            "throughput" => Some(Performance::Throughput),
            "latency" => Some(Performance::Latency),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Performance::Balanced => "balanced",
            Performance::Throughput => "throughput",
            Performance::Latency => "latency",
        }
    }
}

/// Which mode is in force, from [`PERFORMANCE_ENV`].
///
/// An unparseable value is reported and treated as [`Performance::Balanced`]
/// rather than silently ignored, matching `graphics::resolve` and
/// [`device_profile`]: a switch that looks set and does nothing is the failure
/// this project keeps writing rules against.
pub fn performance() -> Performance {
    let Ok(text) = std::env::var(PERFORMANCE_ENV) else {
        return Performance::Balanced;
    };
    if text.trim().is_empty() {
        return Performance::Balanced;
    }
    match Performance::parse(&text) {
        Some(m) => m,
        None => {
            println!(
                "  flags: {PERFORMANCE_ENV}={text:?} is not a performance mode; using balanced. \
                 Known: balanced, throughput, latency"
            );
            Performance::Balanced
        }
    }
}

/// The layer [`performance`]'s mode contributes, empty on `Balanced`.
///
/// **Until this existed, [`performance_flags`] had no callers at all** — the
/// table, its tests and its whole doc comment were in the tree and nothing
/// ever asked for a flag from it, so choosing a mode was not possible by any
/// route. That is the "control that reports success and does not act" shape
/// this codebase keeps finding in its own settings pages, one layer down.
fn performance_layer() -> Layer {
    let mode = performance();
    let values: BTreeMap<String, String> =
        performance_flags(mode, physical_cores()).into_iter().collect();
    if !values.is_empty() {
        println!(
            "  flags: performance mode {} sets {} flag(s) for {} physical core(s)",
            mode.label(),
            values.len(),
            physical_cores()
        );
    }
    Layer { source: Source::Performance, values }
}

/// Physical cores, counted from the kernel's own topology.
///
/// `available_parallelism` reports threads, which is the wrong number here. Each
/// CPU's `topology/core_id` is unique per physical core within a package, so the
/// distinct (package, core) pairs are the count. Falls back to the thread count
/// when `sysfs` is unreadable, which is the only honest answer available then.
pub fn physical_cores() -> usize {
    let mut seen = std::collections::BTreeSet::new();
    if let Ok(dir) = std::fs::read_dir("/sys/devices/system/cpu") {
        for entry in dir.flatten() {
            let base = entry.path().join("topology");
            let read = |name: &str| std::fs::read_to_string(base.join(name)).ok();
            if let (Some(pkg), Some(core)) =
                (read("physical_package_id"), read("core_id"))
            {
                seen.insert((pkg.trim().to_string(), core.trim().to_string()));
            }
        }
    }
    if seen.is_empty() {
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
    } else {
        seen.len()
    }
}

fn builtin_layer() -> Layer {
    Layer {
        source: Source::Builtin,
        values: BUILTIN.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
    }
}

/// Every layer, in precedence order: Cordial's own defaults first so anything
/// can overrule them, then plugins (alphabetically, so the outcome does not
/// depend on directory iteration order), then the user.
pub fn collect() -> Vec<Layer> {
    // The move happens here because this is the one path every reader of the
    // user's overrides goes through, and the runtime has no single startup hook
    // that a launcher-started client and a hand-started one both pass. Once per
    // process: `collect` runs again for every `flags.list` a plugin makes, and
    // re-stating a file that has already moved on each of those is noise in a
    // log people read.
    static MIGRATED: std::sync::Once = std::sync::Once::new();
    MIGRATED.call_once(|| {
        migrate_legacy_user_file(&crate::profile::active());
    });

    let mut layers = vec![builtin_layer(), performance_layer()];

    // System first, so a first-party id is claimed before the user directory is
    // read and a same-id user directory cannot take it. Sorted within each root
    // so the outcome does not depend on directory iteration order.
    let mut seen: std::collections::BTreeMap<String, PathBuf> = Default::default();
    for root in plugin_dirs() {
        let mut ids: Vec<String> = std::fs::read_dir(&root)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        ids.sort();
        for id in ids {
            if let Some(kept) = seen.get(&id) {
                // Loud, and naming both paths, because the quiet version of
                // this is indistinguishable from the plugin simply working.
                println!(
                    "  plugins: ignoring {} because the id {id} is already \
                     provided by {} -- a user plugin may not replace a \
                     first-party one, only be disabled",
                    root.join(&id).display(),
                    kept.display()
                );
                continue;
            }
            seen.insert(id, root.clone());
        }
    }

    // A plugin switched off in Settings contributes no flags either. This was
    // the half the disable switch did not cover: `plugin_host::start_all`
    // consulted `enablement` and refused to spawn the process, while this
    // function went on reading the same plugin's `flags.json` and handing its
    // overrides to the engine -- so "off" meant "its code does not run, but its
    // opinions about the renderer still do". Nobody looking at the switch could
    // have guessed that, which makes it the same class of defect as a stub that
    // reports success.
    let profile = crate::profile::active();
    for (id, root) in seen {
        if !cordial_plugins::enablement::is_enabled(&profile, &id) {
            println!("  plugins: {id} is switched off; its flags.json is not read");
            continue;
        }
        let path = root.join(&id).join("flags.json");
        if let Some(layer) = read_layer(&path, Source::Plugin(id)) {
            layers.push(layer);
        }
    }

    if let Some(layer) = read_layer(&user_path(), Source::User) {
        layers.push(layer);
    }
    layers
}

/// Which device Cordial claims to be when the engine asks — the Android
/// tablet identity it has always sent, or the PC one an experiment wants to
/// try.
///
/// **Why this lives here rather than being a plain `getenv` in the C++ that
/// uses it.** `docs/analysis/flag-init.md` §13 records that mocktail — a
/// comparable third-party client — stays connected to the same place Cordial
/// is disconnected from after 60.6s with reason 304, and that mocktail
/// presents `device profile=pc-windows-11 class=pc model="Windows 11 PC"`
/// where Cordial presents an Android tablet (`native/init_params.cpp`,
/// commit 6d8c280). Whether that causes the 304 is **not established** — it
/// is a difference between a client that works and one that does not, and
/// this makes it a switch rather than a rewrite, so the experiment can be run
/// with a control in the same session.
///
/// The key follows the convention `client_settings.rs`'s `CORDIAL_KEY_PREFIX`
/// set up: a `Cordial`-prefixed name rides this module's layering for its
/// precedence and provenance, and `client_settings.rs::is_roblox_flag`
/// filters it back out before anything reaches Roblox's settings document —
/// the engine has no idea this key exists, same as `CordialGraphicsBackend`.
///
/// **The gap this does not close.** Nothing yet turns a resolved
/// `CordialDeviceProfile` into something `native/init_params.cpp` can see.
/// The C++ side is a separately-compiled translation unit with no live call
/// into this module; the one existing bridge for a comparable case —
/// `CORDIAL_ENGINE_VERSION`, set with `std::env::set_var` right before the
/// init-params call — lives in `crates/cordial-runtime/src/bin/load.rs`,
/// which this change does not touch. So today `device_profile` is reachable
/// from a `flags.json` entry and from tests, but the only thing that actually
/// reaches the engine is the environment variable [`DEVICE_PROFILE_ENV`]
/// itself, read directly by `native/init_params.cpp`'s `presenting_as_pc`.
/// Wiring `flags.json` through to it is exactly the `load.rs` change
/// described above, left for whoever owns that file next.
pub const DEVICE_PROFILE_KEY: &str = "CordialDeviceProfile";

/// The environment variable that actually reaches the engine today, read
/// directly by `native/init_params.cpp`. Named to match [`DEVICE_PROFILE_KEY`]
/// so the two are recognisably the same switch, not two different ones.
pub const DEVICE_PROFILE_ENV: &str = "CORDIAL_DEVICE_PROFILE";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceProfile {
    /// The bare Roblox app token and nothing else, the shape Sober sends —
    /// `RobloxApp/<version>(GlobalDist; Cordial)`, with no `Mozilla`, no
    /// WebKit clause, no platform words and no device block.
    ///
    /// **Not the default.** It is the only one of the three that does not claim
    /// a form factor Cordial has not got, and that is an argument rather than a
    /// measurement; the default stays `PcWindows11` until a frame rate says
    /// otherwise. See
    /// `native/init_params.cpp`'s `device_identity` for the measurement that
    /// says what each identity makes roblox.com serve, and for the
    /// `WebViewUserAgent` value it was copied from.
    RobloxApp,
    /// An Android tablet: `ROBLOX Android App … Tablet Hybrid()`, plus the
    /// device block, plus `InitParams.isTablet = true`.
    ///
    /// **The only identity that asserts a mobile form factor to the engine**,
    /// through `isTablet` — which is a field the engine reads, unlike the
    /// User-Agent, whose effect on graphics tiering nothing here has
    /// established. Reach for this one when mobile-tier defaults are what is
    /// actually wanted, and measure it.
    AndroidTablet,
    /// mocktail's identity, spelled `pc-windows-11` to match its own log line
    /// verbatim rather than a name invented here. **The default, since
    /// 2026-08-20.**
    ///
    /// Worth knowing before choosing it: roblox.com reads this one as
    /// `data-app-type="uwp"` — a Microsoft Store app, which Cordial is not.
    PcWindows11,
}

impl DeviceProfile {
    pub fn parse(text: &str) -> Option<DeviceProfile> {
        match text.trim().to_ascii_lowercase().as_str() {
            "" | "pc" | "pc-windows-11" | "windows" | "windows-11" => {
                Some(DeviceProfile::PcWindows11)
            }
            "roblox-app" | "app" | "roblox" => Some(DeviceProfile::RobloxApp),
            "android" | "android-tablet" | "tablet" => Some(DeviceProfile::AndroidTablet),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            DeviceProfile::RobloxApp => "roblox-app",
            DeviceProfile::AndroidTablet => "android-tablet",
            DeviceProfile::PcWindows11 => "pc-windows-11",
        }
    }
}

impl Default for DeviceProfile {
    /// **`PcWindows11` since 2026-08-20**, `AndroidTablet` before that.
    ///
    /// `RobloxApp` was added on 2026-08-22 and deliberately did **not** take
    /// the default with it: a user had already reported a tablet identity
    /// breaking PC features, and nothing has measured what the bare token does
    /// to a frame rate.
    ///
    /// This must stay in step with `device_identity()` in
    /// `native/init_params.cpp`, which is the one that actually reaches the
    /// engine -- see [`DEVICE_PROFILE_KEY`] for why the flag layer does not.
    /// Two defaults that disagree is the same drift `cordial_build_user_agent`
    /// exists to prevent, and it would be invisible: this side would report one
    /// identity in the log while the engine sent the other.
    fn default() -> Self {
        DeviceProfile::PcWindows11
    }
}

/// What the flag layers say about [`DEVICE_PROFILE_KEY`], if anything.
///
/// Split from [`device_profile`] so a test can supply a resolved map directly
/// rather than writing through the filesystem, matching how this file's other
/// tests exercise `resolve` on synthetic layers.
fn device_profile_from_resolved(resolved: &BTreeMap<String, Resolved>) -> Option<DeviceProfile> {
    resolved.get(DEVICE_PROFILE_KEY).and_then(|r| DeviceProfile::parse(&r.value))
}

/// Resolve which device identity is in force.
///
/// The environment variable wins, because it is the one thing that reliably
/// reaches `native/init_params.cpp` today (see [`DEVICE_PROFILE_KEY`]'s
/// doc for why the flag-layer path does not, yet) — checking it first rather
/// than only in C++ means a caller in this crate gets the same answer the
/// engine will. An unparseable value is reported and treated as the default
/// rather than silently ignored, matching `graphics::resolve`'s reasoning: a
/// switch that looks set but does nothing is the failure this exists to
/// avoid.
pub fn device_profile() -> DeviceProfile {
    if let Ok(text) = std::env::var(DEVICE_PROFILE_ENV) {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            match DeviceProfile::parse(trimmed) {
                Some(p) => return p,
                None => {
                    // The wording and the fallback both match
                    // `native/init_params.cpp`'s `device_identity` exactly.
                    // They used to disagree -- this side fell back to
                    // android-tablet while the C++ fell back to the PC
                    // identity -- so a misspelt value produced a client whose
                    // own log named one identity while the engine was sent
                    // another. Both now name pc-windows-11 and both mean it.
                    println!(
                        "  flags: {DEVICE_PROFILE_ENV}={text:?} is not a device profile; \
                         using pc-windows-11. Known: roblox-app, android-tablet, \
                         pc-windows-11"
                    );
                    return DeviceProfile::PcWindows11;
                }
            }
        }
    }
    device_profile_from_resolved(&resolve(collect())).unwrap_or_default()
}

/// Report what was resolved, naming every conflict.
///
/// Printed rather than silent because a flag doing nothing — because something
/// else set it — is otherwise indistinguishable from a flag that had no effect.
pub fn report(resolved: &BTreeMap<String, Resolved>) {
    if resolved.is_empty() {
        return;
    }
    let conflicts: Vec<_> = resolved.iter().filter(|(_, r)| !r.overridden.is_empty()).collect();
    println!("  flags: {} override(s) applied", resolved.len());
    for (key, r) in conflicts {
        let losers = r
            .overridden
            .iter()
            .map(|(s, v)| format!("{}={v}", s.describe()))
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "  flags: {key} = {} from {} (overrides {losers})",
            r.value,
            r.source.describe()
        );
    }

}

#[cfg(test)]
pub(crate) mod tests {

    /// **Cordial tells games it speaks BloxstrapRPC, and the user can retract
    /// it.** Both halves matter. Without the flag a game that gates its
    /// presence output on it prints nothing and the whole BloxstrapRPC reader
    /// idles; and because the claim is a fingerprinting signal -- a game can
    /// tell it is under a third-party launcher -- somebody who does not want
    /// to make it must be able to say so, which works only if this sits in the
    /// lowest-precedence layer.
    #[test]
    fn the_bloxstrap_protocol_flag_is_a_default_the_user_can_overrule() {
        let builtin = super::builtin_layer();
        assert_eq!(
            builtin.values.get("FFlagUserLaunchedWithBloxstrap").map(String::as_str),
            Some("True"),
            "games gate their presence output on this"
        );

        let mut mine = std::collections::BTreeMap::new();
        mine.insert("FFlagUserLaunchedWithBloxstrap".to_string(), "False".to_string());
        let resolved = super::resolve(vec![
            builtin,
            super::Layer { source: super::Source::User, values: mine },
        ]);
        assert_eq!(
            resolved["FFlagUserLaunchedWithBloxstrap"].value, "False",
            "the user layer must win, or the default is not opt-out"
        );
    }
    use super::*;

    /// **What the editor writes, the loader reads back identically.**
    ///
    /// The one test that has to live here rather than beside the parser in
    /// `cordial_plugins::flag_document`: it spans both crates. The window
    /// validates and writes through that module; the client reads through
    /// `read_layer` in this one. Nothing else asserts that those two agree,
    /// and them disagreeing is the failure the shared module exists to stop --
    /// a page that says "Saved 40 flags" about a file the client then reports
    /// as malformed and ignores.
    #[test]
    fn a_written_document_round_trips_through_the_loader() {
        let dir = std::env::temp_dir().join(format!("cordial-flags-rt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let values = parse_user_text(r#"{"FFlagA": "True", "DFIntB": 12}"#).unwrap();
        write_user_layer(&dir, &values).expect("write");

        let layer = read_layer(&user_path_in(&dir), Source::User).expect("read back");
        assert_eq!(layer.values, values);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `read_layer` must not prefer one value shape over another: an `FLog`
    /// channel's own type, bare number or severity name, is not something
    /// this function can see (see its doc comment). A bare digit and a
    /// severity name written as a JSON string both have to survive
    /// unchanged, or a future "helpful" normalisation could reintroduce
    /// exactly the confusion `docs/analysis/flag-init.md` §22 records.
    #[test]
    fn a_flog_channels_value_survives_whichever_shape_it_was_written_in() {
        let dir = std::env::temp_dir().join("cordial-flags-test-flog-shape");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("flags.json");
        std::fs::write(
            &path,
            r#"{"FLogNativeDM":"Verbose","FLogAppShellReporter":9,"FLogNetwork":"7"}"#,
        )
        .unwrap();

        let layer = read_layer(&path, Source::User).expect("valid JSON should read");
        assert_eq!(layer.values["FLogNativeDM"], "Verbose");
        assert_eq!(layer.values["FLogAppShellReporter"], "9");
        assert_eq!(layer.values["FLogNetwork"], "7");
    }

    fn layer(source: Source, pairs: &[(&str, &str)]) -> Layer {
        Layer {
            source,
            values: pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        }
    }

    #[test]
    fn the_user_beats_a_plugin() {
        let r = resolve(vec![
            layer(Source::Plugin("fps".into()), &[("DFFlagX", "false")]),
            layer(Source::User, &[("DFFlagX", "true")]),
        ]);
        assert_eq!(r["DFFlagX"].value, "true");
        assert_eq!(r["DFFlagX"].source, Source::User);
    }

    #[test]
    fn a_losing_value_is_kept_so_the_conflict_can_be_explained() {
        let r = resolve(vec![
            layer(Source::Plugin("fps".into()), &[("DFFlagX", "false")]),
            layer(Source::User, &[("DFFlagX", "true")]),
        ]);
        assert_eq!(
            r["DFFlagX"].overridden,
            vec![(Source::Plugin("fps".into()), "false".to_string())]
        );
    }

    #[test]
    fn two_plugins_disagreeing_is_recorded_not_hidden() {
        let r = resolve(vec![
            layer(Source::Plugin("a".into()), &[("FIntQ", "1")]),
            layer(Source::Plugin("b".into()), &[("FIntQ", "2")]),
        ]);
        // Deterministic — later layer wins — but the disagreement survives.
        assert_eq!(r["FIntQ"].value, "2");
        assert_eq!(r["FIntQ"].overridden.len(), 1);
        assert_eq!(r["FIntQ"].overridden[0].0, Source::Plugin("a".into()));
    }

    #[test]
    fn untouched_flags_carry_no_conflict() {
        let r = resolve(vec![layer(Source::User, &[("FFlagY", "true")])]);
        assert!(r["FFlagY"].overridden.is_empty());
        assert_eq!(r["FFlagY"].source, Source::User);
    }

    #[test]
    fn the_device_profile_key_carries_the_cordial_prefix_client_settings_filters_on() {
        // client_settings.rs's `is_roblox_flag` keeps this out of Roblox's
        // settings document by checking exactly this prefix. If this constant
        // ever drifted from "Cordial", the key would silently start reaching
        // the engine's own FastFlag document instead of staying Cordial's.
        assert!(DEVICE_PROFILE_KEY.starts_with("Cordial"));
    }

    #[test]
    fn device_profile_parses_every_spelling_and_nothing_else() {
        // The empty string means "the default", and the default is the PC
        // identity -- this used to assert the tablet, which was the drift
        // against `native/init_params.cpp` that the same change fixed.
        assert_eq!(DeviceProfile::parse(""), Some(DeviceProfile::PcWindows11));
        assert_eq!(DeviceProfile::parse("android-tablet"), Some(DeviceProfile::AndroidTablet));
        assert_eq!(DeviceProfile::parse("PC-Windows-11"), Some(DeviceProfile::PcWindows11));
        assert_eq!(DeviceProfile::parse("windows"), Some(DeviceProfile::PcWindows11));
        assert_eq!(DeviceProfile::parse("roblox-app"), Some(DeviceProfile::RobloxApp));
        assert_eq!(DeviceProfile::parse(" Roblox "), Some(DeviceProfile::RobloxApp));
        assert_eq!(DeviceProfile::parse("ps5"), None);
    }

    #[test]
    fn the_default_profile_is_the_one_the_engine_gets_with_nothing_set() {
        // `native/init_params.cpp`'s `device_identity` returns PcWindows11 for
        // an absent `CORDIAL_DEVICE_PROFILE`, and this side must agree or the
        // client's own log names one identity while the engine is sent
        // another -- which is exactly what happened for unrecognised values
        // until 2026-08-22.
        assert_eq!(DeviceProfile::default(), DeviceProfile::PcWindows11);
    }

    #[test]
    fn an_absent_device_profile_flag_resolves_to_nothing_at_all() {
        // Nothing rather than a default, so the caller can tell "no layer
        // asked for an identity" from "a layer asked for the default one" --
        // `device_profile` is where the default is applied.
        let resolved = resolve(vec![layer(Source::User, &[("FFlagUnrelated", "true")])]);
        assert_eq!(device_profile_from_resolved(&resolved), None);
    }

    #[test]
    fn the_pc_identity_is_reachable_through_the_flag_layer_by_its_cordial_key() {
        let resolved =
            resolve(vec![layer(Source::User, &[(DEVICE_PROFILE_KEY, "pc-windows-11")])]);
        assert_eq!(device_profile_from_resolved(&resolved), Some(DeviceProfile::PcWindows11));
    }

    #[test]
    fn the_users_flag_file_beats_a_plugin_asking_for_the_other_identity() {
        // Same precedence rule as everything else in this module: the user's
        // own choice is the one thing a plugin must not override.
        let resolved = resolve(vec![
            layer(Source::Plugin("fps".into()), &[(DEVICE_PROFILE_KEY, "pc-windows-11")]),
            layer(Source::User, &[(DEVICE_PROFILE_KEY, "android-tablet")]),
        ]);
        assert_eq!(device_profile_from_resolved(&resolved), Some(DeviceProfile::AndroidTablet));
    }

    #[test]
    fn the_environment_variable_is_the_one_that_actually_reaches_the_engine_today() {
        // Mutex'd for the same reason `scratch` is: env vars are process-wide
        // and cargo runs tests in parallel threads of the one process. Only
        // the set-variable cases are exercised here — with it unset,
        // `device_profile` falls through to `collect()`, which reads real
        // profile directories; that fallthrough is already covered without
        // touching the filesystem by `device_profile_from_resolved` above, so
        // this test never needs to depend on what happens to be on disk.
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());

        std::env::set_var(DEVICE_PROFILE_ENV, "pc-windows-11");
        assert_eq!(device_profile(), DeviceProfile::PcWindows11);

        std::env::set_var(DEVICE_PROFILE_ENV, "roblox-app");
        assert_eq!(device_profile(), DeviceProfile::RobloxApp);

        // An unparseable value falls back to the **default**, which is what
        // the C++ does. It used to fall back to the tablet here while
        // `device_identity` fell back to the PC identity, so `amiga-500`
        // produced a client whose log and whose engine disagreed.
        std::env::set_var(DEVICE_PROFILE_ENV, "amiga-500");
        assert_eq!(device_profile(), DeviceProfile::PcWindows11);

        std::env::remove_var(DEVICE_PROFILE_ENV);
    }

    /// `XDG_CONFIG_HOME`, `CORDIAL_FLAGS` and `CORDIAL_PLUGIN_DIR` are
    /// process-wide, and cargo runs these in parallel threads of one process.
    /// Same reasoning as `profile`'s own test mutex, which records that the
    /// unserialised version passed anyway on its first run.
    ///
    /// `pub(crate)` so `plugin_host.rs`'s tests can take the same lock before
    /// touching `CORDIAL_PLUGIN_DIR` themselves — two module-local mutexes
    /// guarding one process-wide variable would not actually exclude each
    /// other, which is the flake this is written to avoid rather than repeat.
    pub(crate) static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn scratch(tag: &str) -> (PathBuf, std::sync::MutexGuard<'static, ()>) {
        let guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("cordial-flags-test-{tag}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::env::remove_var("CORDIAL_FLAGS");
        std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
        (root, guard)
    }

    #[test]
    fn one_profiles_overrides_are_not_another_profiles() {
        // A flag set while debugging on a test account used to be set on the
        // account someone plays, because there was one file for the machine.
        let (root, _g) = scratch("isolation");
        let alt = root.join("profiles/alt");
        let main = root.join("profiles/main");
        std::fs::create_dir_all(&alt).unwrap();
        std::fs::create_dir_all(&main).unwrap();
        std::fs::write(user_path_in(&alt), r#"{"DFFlagDebugSomething":"true"}"#).unwrap();

        assert!(read_layer(&user_path_in(&alt), Source::User).is_some());
        assert!(
            read_layer(&user_path_in(&main), Source::User).is_none(),
            "an override made in another profile must not apply here"
        );
    }

    #[test]
    fn a_pre_existing_global_overrides_file_is_moved_into_the_profile() {
        // Leaving it to be ignored would look exactly like overrides never
        // having worked, which is a bug report about the wrong thing.
        let (root, _g) = scratch("migrate");
        let profile = root.join("profiles/default");
        std::fs::create_dir_all(&profile).unwrap();
        let legacy = legacy_user_path();
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, r#"{"FIntTaskSchedulerAutoThreadLimit":"8"}"#).unwrap();

        let moved = migrate_legacy_user_file(&profile).expect("the legacy file should have moved");
        assert_eq!(moved, user_path_in(&profile));
        assert!(!legacy.exists(), "the old file should be gone, not copied");
        let layer = read_layer(&moved, Source::User).expect("the moved file should read");
        assert_eq!(layer.values["FIntTaskSchedulerAutoThreadLimit"], "8");
    }

    #[test]
    fn migration_never_overwrites_overrides_this_profile_already_has() {
        let (root, _g) = scratch("migrate-existing");
        let profile = root.join("profiles/alt");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(user_path_in(&profile), r#"{"FIntQ":"1"}"#).unwrap();

        let legacy = legacy_user_path();
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, r#"{"FIntQ":"2"}"#).unwrap();

        assert!(migrate_legacy_user_file(&profile).is_none());
        let layer = read_layer(&user_path_in(&profile), Source::User).unwrap();
        assert_eq!(layer.values["FIntQ"], "1", "the profile's own file must win");
        assert!(legacy.exists(), "and the old file is left untouched");
    }

    fn scratch_plugin_dir(tag: &str) -> (PathBuf, std::sync::MutexGuard<'static, ()>) {
        let guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("cordial-flags-plugin-write-test-{tag}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::env::set_var("CORDIAL_PLUGIN_DIR", &root);
        (root, guard)
    }

    #[test]
    fn a_plugin_can_write_its_own_flags_layer_and_read_it_back() {
        let (root, _g) = scratch_plugin_dir("roundtrip");
        let values: BTreeMap<String, String> =
            [("FFlagFoo".to_string(), "true".to_string())].into_iter().collect();
        write_plugin_layer("tuner", &values).unwrap();

        let layer = read_layer(&root.join("tuner/flags.json"), Source::Plugin("tuner".into()))
            .expect("the written file should read back");
        assert_eq!(layer.values["FFlagFoo"], "true");
    }

    #[test]
    fn writing_a_plugins_flags_replaces_rather_than_merges() {
        // The plugin is the only writer of its own file, so it must be able
        // to drop a flag it has stopped wanting overridden — a merge would
        // leave it there with nothing able to remove it.
        let (root, _g) = scratch_plugin_dir("replace");
        let first: BTreeMap<String, String> =
            [("FFlagA".to_string(), "true".to_string()), ("FFlagStale".to_string(), "true".to_string())]
                .into_iter()
                .collect();
        write_plugin_layer("tuner", &first).unwrap();
        let second: BTreeMap<String, String> = [("FFlagA".to_string(), "false".to_string())].into_iter().collect();
        write_plugin_layer("tuner", &second).unwrap();

        let layer = read_layer(&root.join("tuner/flags.json"), Source::Plugin("tuner".into())).unwrap();
        assert_eq!(layer.values["FFlagA"], "false");
        assert!(layer.values.get("FFlagStale").is_none(), "{:?}", layer.values);
    }

    #[test]
    fn a_plugin_id_that_is_not_valid_is_refused_rather_than_used_as_a_path() {
        let (_root, _g) = scratch_plugin_dir("bad-id");
        let values = BTreeMap::new();
        for bad in ["..", "../../etc", "a/b", "/etc/passwd"] {
            assert!(write_plugin_layer(bad, &values).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn an_oversized_flags_layer_is_refused() {
        let (_root, _g) = scratch_plugin_dir("oversized");
        let huge: BTreeMap<String, String> =
            [("FFlagHuge".to_string(), "x".repeat(MAX_PLUGIN_FLAGS_BYTES + 1))].into_iter().collect();
        assert!(write_plugin_layer("tuner", &huge).is_err());
    }

    #[test]
    fn one_plugins_write_does_not_touch_another_plugins_flags_json() {
        let (root, _g) = scratch_plugin_dir("isolation");
        let a: BTreeMap<String, String> = [("FFlagA".to_string(), "true".to_string())].into_iter().collect();
        let b: BTreeMap<String, String> = [("FFlagB".to_string(), "true".to_string())].into_iter().collect();
        write_plugin_layer("plugin-a", &a).unwrap();
        write_plugin_layer("plugin-b", &b).unwrap();

        let layer_a = read_layer(&root.join("plugin-a/flags.json"), Source::Plugin("plugin-a".into())).unwrap();
        let layer_b = read_layer(&root.join("plugin-b/flags.json"), Source::Plugin("plugin-b".into())).unwrap();
        assert!(layer_a.values.get("FFlagB").is_none());
        assert!(layer_b.values.get("FFlagA").is_none());
    }

    /// A user plugin may not take an id a first-party plugin already provides.
    ///
    /// The failure guarded here is not a wrong flag value; it is a plugin the
    /// user did not install answering as one they trust. Cordial ships the
    /// web-view interceptor that decides a Studio link means the software
    /// centre, and a same-id directory in a writable location must not become
    /// that. Disabling a first-party plugin stays a separate, legitimate thing.
    #[test]
    fn a_user_plugin_cannot_shadow_a_first_party_id() {
        let (root, _g) = scratch("shadow");
        let sys = root.join("sys");
        let usr = root.join("usr");
        std::fs::create_dir_all(sys.join("studio-links")).unwrap();
        std::fs::create_dir_all(usr.join("studio-links")).unwrap();
        std::fs::create_dir_all(usr.join("mine")).unwrap();
        std::fs::write(sys.join("studio-links/flags.json"), r#"{"FFlagFirstParty":"true"}"#)
            .unwrap();
        std::fs::write(usr.join("studio-links/flags.json"), r#"{"FFlagImposter":"true"}"#)
            .unwrap();
        std::fs::write(usr.join("mine/flags.json"), r#"{"FFlagMine":"true"}"#).unwrap();
        // SAFETY: `scratch` holds the lock these tests share for process env.
        unsafe {
            std::env::set_var("CORDIAL_SYSTEM_PLUGIN_DIR", &sys);
            std::env::set_var("CORDIAL_PLUGIN_DIR", &usr);
        }

        let resolved = resolve(collect());

        unsafe {
            std::env::remove_var("CORDIAL_SYSTEM_PLUGIN_DIR");
            std::env::remove_var("CORDIAL_PLUGIN_DIR");
        }
        assert!(resolved.contains_key("FFlagFirstParty"), "the system plugin should load");
        assert!(
            !resolved.contains_key("FFlagImposter"),
            "a user plugin must not take a first-party id"
        );
        assert!(resolved.contains_key("FFlagMine"), "a user plugin with its own id still loads");
    }

}

#[cfg(test)]
mod performance_tests {
    use super::*;

    #[test]
    fn balanced_changes_nothing_at_all() {
        // The default has to be a no-op until somebody measures the others,
        // or shipping this silently changes every user's client.
        assert!(performance_flags(Performance::Balanced, 8).is_empty());
        assert_eq!(Performance::default(), Performance::Balanced);
    }

    #[test]
    fn throughput_scales_with_cores_and_caps_the_async_floor() {
        let small = performance_flags(Performance::Throughput, 2);
        let big = performance_flags(Performance::Throughput, 16);
        let get = |v: &[(String, String)], k: &str| {
            v.iter().find(|(a, _)| a == k).map(|(_, b)| b.clone()).unwrap()
        };
        assert_eq!(get(&small, "FIntTaskSchedulerAutoThreadLimit"), "2");
        assert_eq!(get(&big, "FIntTaskSchedulerAutoThreadLimit"), "16");
        // Capped at three however many cores there are.
        assert_eq!(get(&small, "FIntTaskSchedulerAsyncTasksMinimumThreadCount"), "2");
        assert_eq!(get(&big, "FIntTaskSchedulerAsyncTasksMinimumThreadCount"), "3");
    }

    #[test]
    fn a_machine_reporting_no_cores_still_gets_a_usable_number() {
        // A pool sized zero would ask the engine for no threads at all, which
        // is a worse failure than a small pool. Every *sized* pool therefore
        // floors at one.
        //
        // `FIntTaskSchedulerThreadMin` is exempt and its zero is deliberate:
        // it is a floor rather than a size, and zero means "no floor, let the
        // scheduler decide", which is what mocktail's throughput policy sets.
        // This test asserted otherwise on its first run and was wrong -- the
        // distinction between a pool size and a pool floor is exactly the kind
        // of thing a copied table loses.
        let v = performance_flags(Performance::Throughput, 0);
        for (k, val) in &v {
            if k.starts_with("FInt") && k != "FIntTaskSchedulerThreadMin" {
                assert_ne!(val, "0", "{k} sizes a pool and must not be zero");
            }
        }
        let min = v.iter().find(|(k, _)| k == "FIntTaskSchedulerThreadMin").unwrap();
        assert_eq!(min.1, "0", "the floor stays zero on purpose");
    }

    #[test]
    fn physical_cores_is_at_least_one_and_no_more_than_the_thread_count() {
        let p = physical_cores();
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
        assert!(p >= 1, "{p}");
        assert!(p <= threads, "physical {p} cannot exceed threads {threads}");
    }
}
