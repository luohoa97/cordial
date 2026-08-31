//! The on-disk shape of a flags file, shared by the launcher and the client.
//!
//! **This is in `cordial-plugins` for a dependency reason and not a conceptual
//! one, and that is worth saying plainly so nobody moves it back.**
//! `cordial_runtime::flags` owns everything about flag *layering* -- which file
//! beats which, what a plugin may contribute, how a conflict is reported -- and
//! that is where this logic belongs by subject. It cannot live there, because
//! `cordial-runtime` depends on `cordial-shell` (the client builds its window
//! through the shell's `host_window`), so the settings window cannot depend on
//! the runtime without a cycle. `cordial-plugins` is the crate both already
//! share.
//!
//! **The alternative was two implementations, and that alternative is the bug.**
//! The settings window validates what somebody pasted; the client parses the
//! same file at startup. If those two disagree by one case, the window accepts
//! a document that the client then reports as malformed and ignores -- and the
//! user sees a page that said "Saved 40 flags" and a game with none of them,
//! with nothing anywhere connecting the two. One function, used by both, is
//! what makes `flags::tests::a_written_document_round_trips_through_the_loader`
//! able to assert that at all.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Where a profile's own flags document lives.
///
/// **Here rather than only in `cordial_runtime::flags` for the same reason as
/// everything else in this module: the window and the client must name the same
/// file.** The settings window computed `profile_dir.join("flags.json")`
/// directly in its first draft, which is right until somebody sets
/// `CORDIAL_FLAGS` -- at which point the window edits one file and the client
/// reads another, and the page reports a save that changes nothing.
///
/// `CORDIAL_FLAGS` makes one file serve every profile, so it is a development
/// switch rather than a supported arrangement; it is honoured here anyway,
/// because a switch that half the program obeys is worse than one nothing does.
pub fn path_in(profile_dir: &Path) -> PathBuf {
    std::env::var_os("CORDIAL_FLAGS")
        .map(PathBuf::from)
        .unwrap_or_else(|| profile_dir.join("flags.json"))
}

/// Parse the text of a flags document into name/value pairs.
///
/// Bloxstrap's exports are a flat object of string values, so they paste in
/// unchanged. Booleans and numbers are converted to their string form rather
/// than refused, because Roblox stores every setting as a string and a person
/// hand-writing the file should not have to know that -- and because
/// `cordial_runtime::flags::read_layer` has always converted them, so refusing
/// here would make the editor stricter than the loader for no reason.
///
/// What is refused is a value with no sensible string form: an object, an
/// array, or a null. **Refused by name**, because "invalid JSON" against a
/// two-hundred-line paste makes somebody bisect their own document by hand.
///
/// An empty document is `Ok` and empty. That is how the editor clears the file,
/// and it is a different outcome from a parse failure.
pub fn parse(text: &str) -> Result<BTreeMap<String, String>, String> {
    if text.trim().is_empty() {
        return Ok(BTreeMap::new());
    }
    let parsed: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
    let obj = parsed
        .as_object()
        .ok_or_else(|| "the document must be a JSON object of flag names to values".to_string())?;

    let mut values = BTreeMap::new();
    for (key, value) in obj {
        if key.trim().is_empty() {
            return Err("a flag with an empty name".to_string());
        }
        let text = match value {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Bool(_) | serde_json::Value::Number(_) => value.to_string(),
            serde_json::Value::Null => {
                return Err(format!("{key}: null is not a value. Remove the line to unset it."))
            }
            serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
                return Err(format!("{key}: a flag value must be text, a number or true/false"))
            }
        };
        values.insert(key.clone(), text);
    }
    Ok(values)
}

/// Write a flags document to `path`, replacing whatever was there.
///
/// Through a temporary and a rename, like
/// `cordial_runtime::flags::write_plugin_layer`: the client reads this file at
/// startup and a half-written document is reported as malformed and ignored,
/// which loses every flag rather than the one being edited.
///
/// An empty set writes `{}` rather than deleting the file. A file that is
/// present and empty says somebody cleared it; an absent one says nothing, and
/// the two are worth telling apart when a flag has stopped working.
pub fn write(path: &Path, values: &BTreeMap<String, String>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(values).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.new");
    std::fs::write(&tmp, format!("{text}\n")).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A Bloxstrap export pastes in unchanged.** That is the whole point of
    /// the editor, and it is a claim about someone else's format, so it gets a
    /// test rather than a comment.
    #[test]
    fn a_bloxstrap_style_document_parses() {
        let text = r#"{
            "DFIntTaskSchedulerTargetFps": "180",
            "FFlagDebugDisplayFPS": "False",
            "DFIntTextureQualityOverride": "3"
        }"#;
        let values = parse(text).expect("a Bloxstrap export must parse");
        assert_eq!(values.get("DFIntTaskSchedulerTargetFps").map(String::as_str), Some("180"));
        assert_eq!(values.get("FFlagDebugDisplayFPS").map(String::as_str), Some("False"));
        assert_eq!(values.len(), 3);
    }

    /// JSON types are converted, matching what the loader has always done.
    #[test]
    fn the_editor_accepts_everything_the_loader_does() {
        let values = parse(r#"{"A": true, "B": 7, "C": "x"}"#).unwrap();
        assert_eq!(values.get("A").map(String::as_str), Some("true"));
        assert_eq!(values.get("B").map(String::as_str), Some("7"));
        assert_eq!(values.get("C").map(String::as_str), Some("x"));
    }

    /// A refusal names the flag it choked on.
    #[test]
    fn a_value_with_no_string_form_is_refused_by_name() {
        let e = parse(r#"{"Good": "1", "Bad": {"nested": 1}}"#).unwrap_err();
        assert!(e.contains("Bad"), "{e}");
        let e = parse(r#"{"Nulled": null}"#).unwrap_err();
        assert!(e.contains("Nulled"), "{e}");
        let e = parse(r#"{"Listy": [1,2]}"#).unwrap_err();
        assert!(e.contains("Listy"), "{e}");
    }

    /// Clearing is not a parse failure.
    #[test]
    fn an_empty_document_clears_rather_than_failing() {
        assert!(parse("").unwrap().is_empty());
        assert!(parse("   \n  ").unwrap().is_empty());
        assert!(parse("{}").unwrap().is_empty());
    }

    /// Not an object is refused before anything else looks at it.
    #[test]
    fn a_document_that_is_not_an_object_is_refused() {
        for text in ["[1,2,3]", "\"a string\"", "42", "true", "{oops"] {
            assert!(parse(text).is_err(), "{text} must be refused");
        }
    }

    /// The write is atomic in the sense that matters: no reader ever sees the
    /// destination half-written, because it is renamed into place.
    #[test]
    fn writing_replaces_rather_than_appends() {
        let dir = std::env::temp_dir().join(format!("cordial-flagdoc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("flags.json");

        write(&path, &parse(r#"{"A": "1", "B": "2"}"#).unwrap()).unwrap();
        write(&path, &parse(r#"{"C": "3"}"#).unwrap()).unwrap();

        let back = parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(back.len(), 1, "the second write must replace the first");
        assert_eq!(back.get("C").map(String::as_str), Some("3"));
        assert!(!dir.join("flags.json.new").exists(), "the temporary must not be left behind");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
