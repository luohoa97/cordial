//! The runtime manifest, `runtime.json` (spec section 1), and the expansion of
//! its argument placeholders.
//!
//! The manifest is advertisement only. The handshake is the truth, and the
//! launcher's keyring entries and report use the manifest's `id`, never the one
//! the runtime claims while running ([`Manifest::check_identity`]).

use crate::error::Violation;
use crate::msg::HelloReply;
use crate::version::{spec_major, Capabilities, Protocol};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Component, Path, PathBuf};

/// The file name a launcher looks for in a runtime's directory.
pub const FILE_NAME: &str = "runtime.json";

/// Who manages Roblox builds for this runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Builds {
    /// The launcher's store picks one and `{build_dir}` is that entry.
    Cordial,
    /// The runtime fetches and keeps its own, and the launcher shows no
    /// Version row for it.
    Runtime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Launch {
    /// The program, then any fixed arguments. `exec[0]` is relative to the
    /// manifest's own directory.
    pub exec: Vec<String>,
    /// Arguments with placeholders (see [`PLACEHOLDERS`]).
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// `"cordial.runtime/<major>"`: the protocol major, written once. The minor
    /// lives only in the handshake, so the two cannot disagree.
    pub spec: String,
    pub id: String,
    pub name: String,
    pub version: String,
    /// Host architectures the runtime runs on, as `std::env::consts::ARCH`
    /// spells them. The launcher hides a runtime on any other and offers no
    /// translation.
    pub arch: Vec<String>,
    pub launch: Launch,
    pub builds: Builds,
    /// Same shape as the handshake: name to integer version.
    pub capabilities: Capabilities,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub support_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub licence: Option<String>,
}

/// The placeholders an `args` entry may use. Each is substituted as a single
/// argument, never through a shell.
pub const PLACEHOLDERS: [&str; 5] = ["socket", "session_dir", "profile_dir", "build_dir", "join_url"];

/// The values the launcher substitutes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Placeholders {
    pub socket: String,
    pub session_dir: String,
    pub profile_dir: String,
    /// Set when `builds` is `cordial`.
    pub build_dir: Option<String>,
    /// A `roblox-player:` link the launcher has already validated and stripped
    /// of its launch ticket. Set only when there is something to join.
    pub join_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    NotJson(String),
    /// A field the spec requires is missing or the wrong type.
    Field(String),
    Invalid(Violation),
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ManifestError::NotJson(e) => write!(f, "{FILE_NAME} is not JSON: {e}"),
            ManifestError::Field(e) => write!(f, "{FILE_NAME}: {e}"),
            ManifestError::Invalid(v) => write!(f, "{FILE_NAME}: {v}"),
        }
    }
}

impl std::error::Error for ManifestError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpandError {
    /// `{name}` is not one of [`PLACEHOLDERS`].
    Unknown(String),
    /// The placeholder is known and the launcher has no value for it, such as
    /// `{join_url}` when there is nothing to join. The launcher decides whether
    /// that means "do not launch with this runtime" or "drop the argument"; the
    /// codec does not guess, because a dangling `--join` would be worse.
    Unset(&'static str),
}

impl fmt::Display for ExpandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExpandError::Unknown(n) => write!(f, "unknown placeholder {{{n}}}"),
            ExpandError::Unset(n) => write!(f, "placeholder {{{n}}} has no value here"),
        }
    }
}

impl std::error::Error for ExpandError {}

/// A manifest turned into something to spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub program: PathBuf,
    pub args: Vec<String>,
}

impl Manifest {
    /// Parse and validate. A manifest that names a protocol major this crate does
    /// not speak still parses, so a launcher can show it as "needs a newer
    /// Cordial" instead of hiding it; ask [`speaks`](Self::speaks).
    pub fn parse(json: &str) -> Result<Manifest, ManifestError> {
        let m: Manifest = serde_json::from_str(json).map_err(|e| {
            if e.is_syntax() || e.is_eof() {
                ManifestError::NotJson(e.to_string())
            } else {
                ManifestError::Field(e.to_string())
            }
        })?;
        m.validate().map_err(ManifestError::Invalid)?;
        Ok(m)
    }

    fn validate(&self) -> Result<(), Violation> {
        if spec_major(&self.spec).is_none() {
            return Err(Violation::new("spec", "expected cordial.runtime/<major>"));
        }
        // The id names a directory, `runtimes/<id>/`, so it must be one path
        // component and nothing a path can climb out through.
        let id_ok = !self.id.is_empty()
            && self.id.len() <= 128
            && self.id != "."
            && self.id != ".."
            && self.id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        if !id_ok {
            return Err(Violation::new("id", "letters, digits, `.`, `_` and `-` only, at most 128 bytes"));
        }
        if self.name.is_empty() {
            return Err(Violation::new("name", "empty"));
        }
        if self.arch.is_empty() {
            return Err(Violation::new("arch", "at least one architecture"));
        }
        let Some(program) = self.launch.exec.first() else {
            return Err(Violation::new("launch.exec", "empty"));
        };
        let p = Path::new(program);
        if p.is_absolute() || p.components().any(|c| matches!(c, Component::ParentDir | Component::Prefix(_))) {
            return Err(Violation::new("launch.exec[0]", "relative to the manifest's directory, with no `..`"));
        }
        if !self.capabilities.contains_key(crate::msg::caps::REQUIRED) {
            return Err(Violation::new("capabilities", format!("a runtime offers {}", crate::msg::caps::REQUIRED)));
        }
        for (i, arg) in self.launch.args.iter().enumerate() {
            for name in placeholders_in(arg) {
                if !PLACEHOLDERS.contains(&name) {
                    return Err(Violation::new(format!("launch.args[{i}]"), format!("unknown placeholder {{{name}}}")));
                }
            }
        }
        Ok(())
    }

    /// The protocol major the manifest names. `parse` guarantees a spelling.
    pub fn major(&self) -> u32 {
        spec_major(&self.spec).expect("validated in parse")
    }

    /// Whether this crate speaks the manifest's major.
    pub fn speaks(&self) -> bool {
        self.major() == Protocol::CURRENT.major
    }

    /// Whether the runtime runs on `arch`, spelled as `std::env::consts::ARCH`.
    pub fn supports_arch(&self, arch: &str) -> bool {
        self.arch.iter().any(|a| a == arch)
    }

    /// Refuse a handshake whose `runtime.id` differs from this manifest's.
    pub fn check_identity(&self, reply: &HelloReply) -> Result<(), Violation> {
        if reply.runtime.id == self.id {
            Ok(())
        } else {
            Err(Violation::new(
                "runtime.id",
                format!("the handshake says {:?} and the manifest says {:?}", reply.runtime.id, self.id),
            ))
        }
    }

    /// The program path and argument list to spawn, with placeholders expanded.
    /// `dir` is the directory the manifest was read from.
    pub fn command(&self, dir: &Path, with: &Placeholders) -> Result<Command, ExpandError> {
        let mut fixed = self.launch.exec.iter();
        let program = dir.join(fixed.next().expect("validated in parse"));
        let mut args: Vec<String> = fixed.cloned().collect();
        for arg in &self.launch.args {
            args.push(expand(arg, with)?);
        }
        Ok(Command { program, args })
    }
}

/// The names between braces in `arg`, in order, for every `{word}` where the
/// word is lowercase letters and underscores. Other braces are literal text.
fn placeholders_in(arg: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut rest = arg;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) if is_word(&after[..close]) => {
                found.push(&after[..close]);
                rest = &after[close + 1..];
            }
            _ => rest = after,
        }
    }
    found
}

fn is_word(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
}

/// Expand the placeholders in one argument, in a single left-to-right pass.
///
/// **Single pass, so a substituted value is never expanded again.** A join URL
/// that happens to contain `{profile_dir}` arrives as that text. The result is
/// one argument whatever the values hold: spaces and quotes in a path do not
/// split it, because nothing here goes near a shell.
pub fn expand(arg: &str, with: &Placeholders) -> Result<String, ExpandError> {
    let mut out = String::with_capacity(arg.len());
    let mut rest = arg;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) if is_word(&after[..close]) => {
                out.push_str(value_of(&after[..close], with)?);
                rest = &after[close + 1..];
            }
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    Ok(out)
}

fn value_of<'a>(name: &str, with: &'a Placeholders) -> Result<&'a str, ExpandError> {
    match name {
        "socket" => Ok(&with.socket),
        "session_dir" => Ok(&with.session_dir),
        "profile_dir" => Ok(&with.profile_dir),
        "build_dir" => with.build_dir.as_deref().ok_or(ExpandError::Unset("build_dir")),
        "join_url" => with.join_url.as_deref().ok_or(ExpandError::Unset("join_url")),
        other => Err(ExpandError::Unknown(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msg::{ClientIdent, RuntimeIdent};

    /// The manifest from spec section 1, verbatim.
    const SPEC_EXAMPLE: &str = r#"{
  "spec": "cordial.runtime/1",
  "id": "org.example.runtime",
  "name": "Example",
  "version": "0.13",
  "arch": ["x86_64"],
  "launch": {
    "exec": ["bin/run"],
    "args": ["--socket", "{socket}", "--profile", "{profile_dir}"]
  },
  "builds": "runtime",
  "capabilities": { "lifecycle": 1, "events.core": 1, "settings": 1 },
  "support_url": "https://example.org/issues",
  "licence": "MIT"
}"#;

    fn with() -> Placeholders {
        Placeholders {
            socket: "/p/runtime/ab12cd34/ctl.sock".into(),
            session_dir: "/p/runtime/ab12cd34".into(),
            profile_dir: "/home/me/My Profile".into(),
            build_dir: Some("/b/700".into()),
            join_url: Some("roblox-player:1+placeId:1818+x:{socket}".into()),
        }
    }

    #[test]
    fn the_spec_example_parses_and_expands() {
        let m = Manifest::parse(SPEC_EXAMPLE).unwrap();
        assert_eq!(m.major(), 1);
        assert!(m.speaks());
        assert_eq!(m.builds, Builds::Runtime);
        assert!(m.supports_arch("x86_64") && !m.supports_arch("aarch64"));
        let c = m.command(Path::new("/data/cordial/runtimes/org.example.runtime"), &with()).unwrap();
        assert_eq!(c.program, PathBuf::from("/data/cordial/runtimes/org.example.runtime/bin/run"));
        assert_eq!(
            c.args,
            vec!["--socket", "/p/runtime/ab12cd34/ctl.sock", "--profile", "/home/me/My Profile"],
            "a path with a space is still one argument"
        );
    }

    #[test]
    fn a_newer_major_still_parses_so_it_can_be_shown_as_needing_a_newer_cordial() {
        let m = Manifest::parse(&SPEC_EXAMPLE.replace("cordial.runtime/1", "cordial.runtime/2")).unwrap();
        assert_eq!(m.major(), 2);
        assert!(!m.speaks());
        assert!(Manifest::parse(&SPEC_EXAMPLE.replace("cordial.runtime/1", "cordial.runtime/x")).is_err());
    }

    #[test]
    fn a_substituted_value_is_never_expanded_again_and_never_split() {
        let m = with();
        assert_eq!(expand("--join={join_url}", &m).unwrap(), "--join=roblox-player:1+placeId:1818+x:{socket}");
        assert_eq!(expand("{socket}:{session_dir}", &m).unwrap(), "/p/runtime/ab12cd34/ctl.sock:/p/runtime/ab12cd34");
        assert_eq!(expand("{profile_dir}", &m).unwrap(), "/home/me/My Profile");
    }

    #[test]
    fn braces_that_are_not_placeholders_are_literal() {
        let m = with();
        for s in ["{}", "{ socket }", "{Socket}", "{{x", "a}b", "{", "}", "x{y"] {
            // `{ socket }` and `{Socket}` are not words in the placeholder alphabet.
            assert_eq!(expand(s, &m).unwrap(), s, "{s:?}");
        }
    }

    #[test]
    fn an_unknown_or_unset_placeholder_is_an_error_not_a_guess() {
        assert_eq!(expand("{nonsense}", &with()), Err(ExpandError::Unknown("nonsense".into())));
        let mut none = with();
        none.join_url = None;
        none.build_dir = None;
        assert_eq!(expand("{join_url}", &none), Err(ExpandError::Unset("join_url")));
        assert_eq!(expand("{build_dir}/x", &none), Err(ExpandError::Unset("build_dir")));
    }

    #[test]
    fn a_manifest_with_an_unknown_placeholder_is_refused_at_parse() {
        let bad = SPEC_EXAMPLE.replace("{profile_dir}", "{profile}");
        assert!(matches!(Manifest::parse(&bad), Err(ManifestError::Invalid(_))));
    }

    #[test]
    fn the_id_is_one_safe_path_component() {
        for bad in ["", ".", "..", "a/b", "../x", "a b", "a\u{0}b"] {
            let m = SPEC_EXAMPLE.replace("org.example.runtime", bad);
            assert!(Manifest::parse(&m).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn exec_stays_inside_the_manifests_directory() {
        for bad in ["/bin/sh", "../run", "a/../../run"] {
            let m = SPEC_EXAMPLE.replace("bin/run", bad);
            assert!(Manifest::parse(&m).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_runtime_must_offer_lifecycle_and_name_an_arch() {
        assert!(Manifest::parse(&SPEC_EXAMPLE.replace("\"lifecycle\": 1, ", "")).is_err());
        assert!(Manifest::parse(&SPEC_EXAMPLE.replace("[\"x86_64\"]", "[]")).is_err());
    }

    #[test]
    fn unknown_fields_are_ignored_and_missing_ones_are_named() {
        let with_extra = SPEC_EXAMPLE.replace("\"licence\": \"MIT\"", "\"licence\": \"MIT\", \"future\": [1]");
        assert!(Manifest::parse(&with_extra).is_ok());
        let missing = SPEC_EXAMPLE.replace("\"builds\": \"runtime\",", "");
        assert!(matches!(Manifest::parse(&missing), Err(ManifestError::Field(_))));
        assert!(matches!(Manifest::parse("{"), Err(ManifestError::NotJson(_))));
        assert!(matches!(
            Manifest::parse(&SPEC_EXAMPLE.replace("\"runtime\"", "\"somebody\"")),
            Err(ManifestError::Field(_))
        ));
    }

    #[test]
    fn the_handshake_may_not_rename_the_runtime() {
        let m = Manifest::parse(SPEC_EXAMPLE).unwrap();
        let reply = |id: &str| HelloReply {
            protocol: Protocol::CURRENT,
            runtime: RuntimeIdent { id: id.into(), version: "0.13".into() },
            client: ClientIdent { name: "Roblox".into(), version: "1".into(), build: "1".into() },
            caps: Capabilities::from([("lifecycle".to_string(), 1)]),
        };
        assert!(m.check_identity(&reply("org.example.runtime")).is_ok());
        assert!(m.check_identity(&reply("org.example.other")).is_err());
    }

    #[test]
    fn a_manifest_round_trips() {
        let m = Manifest::parse(SPEC_EXAMPLE).unwrap();
        assert_eq!(Manifest::parse(&serde_json::to_string(&m).unwrap()).unwrap(), m);
    }
}
