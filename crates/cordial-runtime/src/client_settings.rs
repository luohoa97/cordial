//! Roblox's client settings — the FastFlag set the engine runs on.
//!
//! On Android the *application* fetches this document and hands it to the engine
//! through `NativeGLInterface.nativeInitClientSettings`. Cordial is the host
//! application here, so doing that fetch is the job, not a workaround.
//!
//! **"The engine never fetches it itself" was the rest of this paragraph, and it
//! is wrong.** It rested on breakpoints on `getaddrinfo`, `connect` and
//! `SSL_connect` never being hit during startup, which is a statement about the
//! first second and was read as a statement about the process. The engine runs a
//! `DynamicFastVariableReloader` of its own. Turning the engine's own HTTP
//! tracing on (`DFLogHttpTraceLight`) shows it, on 2.734.0.917, signed in:
//!
//! ```text
//! 2.251  HttpResponse(#22) status:200 bodySize:1305506
//!        url:{ "https://clientsettingscdn.roblox.com/v2/settings-compressed/application/GoogleAndroidApp.zst" }
//! 4.150  [FlagCache] writeFlagCache: Successfully wrote 372673 bytes
//! 120.11 [FLog::DynamicFastVariableReloader] DynamicFastVariableReloader finished flag fetch
//! ```
//!
//! Note the application name: the engine asks for **`GoogleAndroidApp`**, where
//! this file asks for `AndroidApp`. Whether the two documents differ in anything
//! Cordial cares about is **not established**; nobody has diffed them.
//!
//! What that reloader costs is written up on [`apply_overrides`], because it is
//! the thing that decides how long an override survives.
//!
//! ## The call contract, established by experiment
//!
//! `nativeInitClientSettings(String, String, String)` returns an `int`. Feeding
//! it known-good and known-bad documents settles what it means:
//!
//! (An earlier version of this note called that return value "the only
//! trustworthy signal, since the engine's own `FLog` output is not routed
//! anywhere visible in this build". The second half was wrong. `FLog` is routed,
//! and always has been — the engine writes it to `appData/logs/*.log`, relative
//! to the working directory. Read that file; it is the best diagnostic Cordial
//! has.)
//!
//! | first argument | result |
//! |---|---|
//! | the real document, `{"applicationSettings": {...}}` | `0` |
//! | `{"applicationSettings":{"FFlagNotARealFlag":"True"}}` | `0` |
//! | `this is not json at all` | `1` |
//! | `{}` — valid JSON, no `applicationSettings` key | `1` |
//!
//! So **`0` is success**, the document goes in the *first* argument, and the
//! `applicationSettings` wrapper must be kept rather than unwrapped. Passing the
//! document in either of the other two positions returns `1` regardless of
//! whether it is valid, which is what "this argument is not the settings" looks
//! like. One of those two is an overrides document — the engine has a
//! `ParseFailure on overrides` log string — but which is still unestablished, so
//! both are left empty rather than guessed at.
//!
//! Those first readings were confounded: `--client-settings` fed *two* call
//! sites, so `nativeInitializeNativeFlags` was receiving the document too and
//! the result could not be attributed to this call alone. On the automatic path
//! the flag-names call gets nothing, and the discriminator survives cleanly —
//! empty string gives `1`, the real document gives `0`. The conclusion held, but
//! it had not actually been shown until the two were separated.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Roblox's settings CDN. The application name is `AndroidApp`; it is not a
/// guess — `AndroidClient`, `AndroidPlayer`, `AndroidClientSettings` and
/// `AndroidAppSettings` all return HTTP 400 "The application name is invalid",
/// and `AndroidApp` returns the real 1.2 MB document.
const URL: &str = "https://clientsettingscdn.roblox.com/v2/settings/application/AndroidApp";

/// How long a cached copy is used before refetching.
///
/// Roblox changes flags continuously, so this is not "cache forever". It is long
/// enough that ordinary repeat launches do not each hit the network, and short
/// enough that a machine left running picks up changes within a session or two.
const MAX_AGE: Duration = Duration::from_secs(6 * 60 * 60);

fn cache_path() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
        .join("cordial/clientsettings.json")
}

fn fresh(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    let age = SystemTime::now().duration_since(meta.modified().ok()?).ok()?;
    (age < MAX_AGE).then(|| std::fs::read_to_string(path).ok())?
}

/// Looks like a settings document rather than an error page.
///
/// Worth checking before caching: the CDN answers a bad application name with a
/// perfectly well-formed JSON error body, and caching that would produce six
/// hours of failures that look like a flag problem rather than a fetch problem.
fn plausible(body: &str) -> bool {
    body.contains("\"applicationSettings\"")
}

/// Where the document handed to the engine came from, or why there is none.
///
/// This exists to answer the question GitHub issue #21 could not: reporter A's
/// `nativeInitializeNativeFlags` resolved roughly ten of a hundred and
/// thirty-nine flags against a healthy machine's eighty-one, and the log
/// between `nativeSetCacheDirectory ok (early)` and `bootstrapTheApp
/// installed` said nothing about whether that was an unreadable
/// `--client-settings` path, a fetch that never connected, or a fetch that
/// connected and was refused. It is a name for that gap, not a fix for the
/// bug -- see `docs/analysis/flag-init.md` §50 and its correction for why a
/// wait on any of this would hang instead of help: `bootstrapTheApp` itself
/// runs on the engine's own schedule, not synchronously inside
/// `initializeNativeCode` as an earlier draft of that fix assumed.
pub enum Source {
    /// `--client-settings <path>`, and it read.
    Explicit,
    /// The on-disk cache, still inside `MAX_AGE`.
    FreshCache,
    /// A live fetch from the CDN.
    Fetched,
    /// The fetch failed and a stale cache answered in its place.
    StaleCache,
    /// Nothing did. The engine gets an empty document and resolves almost
    /// every flag to its own compiled default -- this is why.
    Nothing(String),
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Source::Explicit => write!(f, "--client-settings"),
            Source::FreshCache => write!(f, "cache"),
            Source::Fetched => write!(f, "fetched"),
            Source::StaleCache => write!(f, "stale cache, fetch failed"),
            Source::Nothing(why) => write!(f, "nothing: {why}"),
        }
    }
}

/// The client settings document, from cache when it is fresh and from Roblox
/// otherwise.
///
/// Returns `None` rather than failing the launch: the engine is given whatever
/// it can be given, and a client that starts without flags is more useful than
/// one that refuses to start because a CDN was unreachable.
pub fn load(explicit: Option<&str>) -> Option<String> {
    load_reporting(explicit).0
}

/// As [`load`], but says where the document came from (or why there is none)
/// rather than discarding that fact. The default (`bootstrapTheApp`) launch
/// path prints it; see `load.rs`'s `BootstrapPlan`.
pub fn load_reporting(explicit: Option<&str>) -> (Option<String>, Source) {
    let (body, source) = load_base(explicit);
    (body.map(apply_overrides), source)
}

fn load_base(explicit: Option<&str>) -> (Option<String>, Source) {
    if let Some(path) = explicit {
        return match std::fs::read_to_string(path) {
            Ok(body) => (Some(body), Source::Explicit),
            // Used to be `.ok()`, silently. An unreadable path given with
            // `--client-settings` is a typo or a stale symlink, not "use the
            // network instead" -- and it looked exactly like a healthy launch
            // with nothing on the other end of it. Printed here too, not only
            // returned as a `Source`, because `load()` -- still used at every
            // call site but the default one -- throws the `Source` away.
            Err(e) => {
                let why = format!("--client-settings {path}: {e}");
                println!("  client settings: {why}");
                (None, Source::Nothing(why))
            }
        };
    }
    let cache = cache_path();
    if let Some(body) = fresh(&cache) {
        return (Some(body), Source::FreshCache);
    }
    match fetch() {
        Ok(body) => {
            if let Some(parent) = cache.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&cache, &body);
            (Some(body), Source::Fetched)
        }
        // A stale copy beats nothing when the network is down.
        Err(why) => match std::fs::read_to_string(&cache) {
            Ok(body) => (Some(body), Source::StaleCache),
            Err(_) => (None, Source::Nothing(why)),
        },
    }
}

/// Keys in the flag layers that belong to Cordial rather than to Roblox.
///
/// One prefix rather than a list, so a second Cordial-owned key does not need
/// this file edited to stay out of the engine's settings.
const CORDIAL_KEY_PREFIX: &str = "Cordial";

/// Whether a resolved key is Roblox's to receive.
fn is_roblox_flag(key: &str) -> bool {
    !key.starts_with(CORDIAL_KEY_PREFIX)
}

/// Merge every layer of flag overrides into the settings document.
///
/// The layering, precedence and provenance live in [`crate::flags`]; this only
/// applies the result. Splitting them is what lets a plugin contribute flags
/// without writing into the user's file.
///
/// This is the mechanism that demonstrably works. Verified with a control:
/// `DFFlagRbxTransportUseRtcioRna=false` removes
/// `Initialized RtcIoRna with 1 event loop threads` from the engine's own log,
/// and the same run without it has that line. `nativePreloadFlagOverrides` is
/// *not* the mechanism despite the name — it was tried with several document
/// shapes and changed nothing observable.
///
/// **It works for about two seconds, and that control only held because the
/// line it watches is printed at 0.375 s.** The engine's own settings fetch
/// (see the module note) lands at 1.6–2.3 s on this machine and *reapplies
/// Roblox's document over the top*, so every `DF*` value Cordial merged in is
/// reverted to Roblox's for any key the fetched document contains. Keys the
/// document does not contain keep Cordial's value for the whole run.
///
/// Measured both directions inside one run (2026-08-20, 2.734.0.917, signed
/// in), which is what makes it a control rather than an anecdote:
///
/// ```text
/// override                     Roblox's value   observed
/// DFLogHttpTraceLight = "7"    "0"              logs 0.65 s .. 2.25 s, then silent
/// DFLogHttpTraceError = "0"    "12"             silent .. 2.25 s, logging again from 4.86 s
/// DFLogHttpTrace      = "7"    absent           logs 0.86 s .. 120.1 s
/// ```
///
/// The middle row is the one that settles it: an override asking for *silence*
/// on a key Roblox sets went loud again, at Roblox's own level, part-way
/// through the run. Nothing Cordial does can be the cause of that.
///
/// Two consequences worth having in mind before trusting a flag experiment
/// here. A `DFFlag`/`DFInt`/`DFString`/`DFLog` override only reliably governs
/// the first couple of seconds, so a claim resting on one needs the effect to
/// be visible in that window or it is measuring Roblox's value. And an
/// `FFlag`/`FInt`/`FString` override is read once at startup and is *not*
/// reverted — the family this file's own module doc, and [`crate::flags`],
/// present as the harder one to influence is in fact the durable one.
///
/// **Not established:** whether the reloader merges or replaces, and whether a
/// later fetch (one was seen at 120.1 s) reverts an override a second time
/// after something else had changed it. Neither was tested.
fn apply_overrides(doc: String) -> String {
    let resolved = crate::flags::resolve(crate::flags::collect());
    if resolved.is_empty() {
        return doc;
    }
    // Cordial's own keys ride the flag layering for its precedence and
    // provenance, and are not Roblox flags — `CordialGraphicsBackend` asks
    // Cordial whether to offer the engine a Vulkan loader, which is a question
    // the engine has no idea it is being asked. Handing them over would put
    // invented names in Roblox's settings document; the engine ignores what it
    // does not know, but a flag it silently ignores is exactly the thing this
    // project keeps mistaking for a flag that works.
    let overrides: serde_json::Map<String, serde_json::Value> = resolved
        .iter()
        .filter(|(k, _)| is_roblox_flag(k))
        .map(|(k, r)| (k.clone(), serde_json::Value::String(r.value.clone())))
        .collect();

    match merge(&doc, overrides) {
        Ok((merged, _)) => {
            crate::flags::report(&resolved);
            merged
        }
        Err(why) => {
            println!("  flags: {why}; ignoring overrides");
            doc
        }
    }
}

/// Merge overrides into `applicationSettings`, returning the document and how
/// many were applied. Split out from layer resolution so it can be tested.
fn merge(
    doc: &str,
    overrides: serde_json::Map<String, serde_json::Value>,
) -> Result<(String, usize), &'static str> {
    let mut parsed: serde_json::Value =
        serde_json::from_str(doc).map_err(|_| "the settings document did not parse")?;
    let app = parsed
        .get_mut("applicationSettings")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or("no applicationSettings object")?;

    let mut applied = 0usize;
    for (k, v) in overrides {
        let as_string = match v {
            serde_json::Value::String(s) => s,
            other => other.to_string(),
        };
        app.insert(k, serde_json::Value::String(as_string));
        applied += 1;
    }
    let out = serde_json::to_string(&parsed).map_err(|_| "the merged document did not serialise")?;
    Ok((out, applied))
}

/// Returns the reason rather than `None` on failure, because `load_base` now
/// has somewhere to put it (`Source::Nothing`) and "why" is exactly what was
/// missing from the report this exists to answer.
///
/// Used to be a bare `ureq::get(URL).call()` with no connect or read timeout
/// configured at all -- everything `None` except the read-size limit below --
/// so a CDN that accepted the TCP connection and then said nothing could hold
/// a launch open indefinitely with no way to tell, from the log, that this was
/// what had happened. `cordial_update::http::get_text` is the "house" client
/// this project already uses for its own version/changelog/APK metadata
/// fetches, built on the timeouts `cordial-update/src/http.rs` picked for
/// exactly this shape of request -- ten seconds to connect, twenty total
/// (`http::CONNECT`, `http::TIMEOUT`). Reused rather than re-derived, so the
/// two numbers cannot drift apart, and it comes with `url_policy`'s
/// host-locked redirect handling for free, which the bare call did not have.
fn fetch() -> Result<String, String> {
    // The document is ~1.2 MB; `get_text`'s own limit is 8 MB, which is a
    // metadata-sized bound rather than the APK-sized one `crate::download`
    // needs, but comfortably above anything this endpoint has ever answered.
    let body = cordial_update::http::get_text(URL).map_err(|e| e.to_string())?;
    if plausible(&body) {
        Ok(body)
    } else {
        Err(format!("{URL} answered with something that is not a settings document"))
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn cordial_owned_keys_do_not_reach_roblox_settings() {
        // `CordialGraphicsBackend` asks Cordial whether to offer the engine a
        // Vulkan loader, which is a question the engine has no idea it is being
        // asked. It rides the flag layering for that machinery's precedence and
        // provenance, not because it is a FastFlag.
        assert!(!is_roblox_flag("CordialGraphicsBackend"));
        assert!(!is_roblox_flag(crate::graphics::KEY));
        // Roblox's own prefixes are untouched. `DFFlag...` matters most: it is
        // the one this file has a measured control for.
        for key in ["DFFlagRbxTransportUseRtcioRna", "FFlagDebugDisplayFPS", "FStringTest"] {
            assert!(is_roblox_flag(key), "{key}");
        }
    }
    use super::*;

    fn map(pairs: &[(&str, serde_json::Value)]) -> serde_json::Map<String, serde_json::Value> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    #[test]
    fn an_override_replaces_an_existing_flag() {
        let doc = r#"{"applicationSettings":{"DFFlagX":"True","FFlagY":"False"}}"#;
        let (out, n) = merge(doc, map(&[("DFFlagX", serde_json::json!(false))])).unwrap();
        assert_eq!(n, 1);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["applicationSettings"]["DFFlagX"], "false");
        // untouched flags survive
        assert_eq!(v["applicationSettings"]["FFlagY"], "False");
    }

    #[test]
    fn non_string_values_are_converted_rather_than_rejected() {
        // Roblox stores every value as a string, so a config file written with
        // a bare `7` or `true` has to work rather than be a silent no-op.
        let doc = r#"{"applicationSettings":{}}"#;
        let (out, _) = merge(
            doc,
            map(&[("FIntA", serde_json::json!(7)), ("FFlagB", serde_json::json!(true))]),
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["applicationSettings"]["FIntA"], "7");
        assert_eq!(v["applicationSettings"]["FFlagB"], "true");
    }

    #[test]
    fn a_document_without_application_settings_is_refused() {
        assert!(merge(r#"{"nope":{}}"#, map(&[("FFlagX", serde_json::json!(1))])).is_err());
    }

    #[test]
    fn an_error_body_is_not_mistaken_for_settings() {
        // What the CDN actually returns for a bad application name. It is valid
        // JSON, so only the shape check distinguishes it.
        let err = r#"{"errors":[{"code":1,"message":"The application name is invalid."}]}"#;
        assert!(!plausible(err));
        assert!(plausible(r#"{"applicationSettings":{"FFlagX":"True"}}"#));
    }

    /// Exercises `load_base` rather than `load` on purpose. `load` merges the
    /// user's own overrides — since ADR-013 that is `<profile>/flags.json`, and
    /// before it `~/.config/cordial/flags.json` — so going through it would
    /// make this test read the developer's real profile and fail for anyone who
    /// has overrides, which is exactly what it did once one existed. The
    /// behaviour under test is path-versus-network, and that is `load_base`.
    #[test]
    fn an_explicit_path_bypasses_the_network() {
        let dir = std::env::temp_dir().join("cordial-cs-test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("settings.json");
        std::fs::write(&p, r#"{"applicationSettings":{}}"#).unwrap();
        let (body, source) = load_base(Some(p.to_str().unwrap()));
        assert_eq!(body.as_deref(), Some(r#"{"applicationSettings":{}}"#));
        assert!(matches!(source, Source::Explicit));
    }

    /// GitHub issue #21, reporter A: an unreadable `--client-settings` path
    /// used to come back as a bare `None`, identical to "the CDN was
    /// unreachable and there was no cache either" -- the two failures a
    /// launch log could not tell apart. `Source::Nothing` exists so it can.
    #[test]
    fn an_unreadable_explicit_path_says_why_rather_than_just_no() {
        let (body, source) = load_base(Some("/nonexistent/cordial-cs-test-path.json"));
        assert_eq!(body, None);
        match source {
            Source::Nothing(why) => assert!(why.contains("/nonexistent/cordial-cs-test-path.json")),
            _ => panic!("expected Source::Nothing, got a different source"),
        }
    }
}
