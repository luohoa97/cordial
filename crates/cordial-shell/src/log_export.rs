//! The log archive a bug report attaches, and the redaction it rests on.
//!
//! Issue threads here keep asking for the same four things by hand -- terminal
//! output, the engine's own log, whether it dumped core, a few health lines --
//! and each ask is a round trip with someone who may not have a terminal. This
//! gathers them into one `.zip` ([`export`]) from the Report a Problem screen.
//!
//! ## Redaction is the requirement, and it is done at export
//!
//! These files are not safe to attach as they stand. The engine's log carries
//! user ids on its join line, the client echoes `[identity]` and `[cookies]`
//! narration, a deep link carries a live auth ticket, and a chat box is a text
//! box like any other. The crash page already withholds the first two at
//! capture (`launch::redact`); an archive is read by strangers on a public
//! issue tracker, so everything here is scrubbed again, on the way out, over
//! all of its sources together.
//!
//! **Together matters.** A user id seen on the engine's join line is replaced
//! by the same `<user-id-1>` wherever else it appears in the archive, because
//! "the same account did both things" is often the diagnosis. That takes two
//! passes: [`Redactor::learn`] over every source, then [`Redactor::redact`].
//!
//! What is removed, and by what rule, in the order applied to each line:
//!
//! - lines about the saved session (`[cookies]`, `[identity]`) and the text of
//!   chat channels (`[FLog::...Chat...]`) are replaced by a marker;
//! - ids and names already learned, wherever they appear; the home directory
//!   as `~`; `/home/<name>` as `/home/<user>`; profile names under
//!   `instances/`, which are routinely the account's name;
//! - the value after any key that names a secret (`.ROBLOSECURITY`, `cookie`,
//!   `ticket`, `token`, `password`, `authorization`, `gameinfo`, ...), a user
//!   id, a username or display name, or typed text (`text=`);
//! - every URL's query string, fragment and `user:pass@`, because an
//!   authentication URL is identified by its query;
//! - any `_|WARNING:` value, which is how Roblox starts a session cookie.
//!
//! This is **pattern matching over text and it will miss a secret nobody has
//! thought of**; the archive's README says so, and the screen tells the user
//! it is redacted rather than that it is safe. The rules are deliberately
//! over-eager -- a harmless `token:` value is lost to keep a real one out.
//!
//! No core file is ever included: a core dump of the client contains its
//! memory, session and all.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::profile::Build;

/// The most of one engine log kept, from its end. A log from a long game can
/// run to tens of megabytes and the interesting part is the tail.
const ENGINE_LOG_BYTES: u64 = 2 * 1024 * 1024;
/// How many engine logs per build.
const ENGINE_LOGS: usize = 2;

const SESSION_MARKER: &str = "    (a line about the saved session was left out)";

/// Names that are words rather than accounts. A user called `user` would be
/// replaced everywhere the word occurs, and these would mangle ordinary lines.
const NOT_A_NAME: &[&str] = &[
    "roblox", "default", "guest", "user", "player", "true", "false", "null", "none", "unknown", "nil", "cordial",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Secret,
    UserId,
    UserName,
    Text,
}

struct Hit {
    start: usize,
    end: usize,
    kind: Kind,
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')
}

/// What a key says about the value after it.
fn classify(ident: &str) -> Option<Kind> {
    let l = ident.to_ascii_lowercase();
    if l.contains("roblosecurity") {
        return Some(Kind::Secret);
    }
    const SECRET: &[&str] = &[
        "ticket",
        "token",
        "secret",
        "password",
        "passwd",
        "cookie",
        "authorization",
        "apikey",
        "api_key",
        "api-key",
        "sessionid",
        "session_id",
        "sessionkey",
        "gameinfo",
        "placelauncherurl",
        "launchdata",
        "credential",
    ];
    if SECRET.iter().any(|s| l.ends_with(s)) {
        return Some(Kind::Secret);
    }
    for s in ["userid", "user_id", "user-id"] {
        if l.ends_with(s) {
            return Some(Kind::UserId);
        }
    }
    for s in ["username", "user_name", "displayname", "display_name", "loginname"] {
        if l.ends_with(s) {
            return Some(Kind::UserName);
        }
    }
    if l == "text" || ["typedtext", "inputtext", "committext", "preedit"].iter().any(|s| l.ends_with(s)) {
        return Some(Kind::Text);
    }
    None
}

/// Every `key: value` or `key=value` in `line` whose key says what the value
/// is. Non-overlapping, in order.
fn keyed_hits(line: &str) -> Vec<Hit> {
    let b = line.as_bytes();
    let mut hits = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if !is_ident(b[i]) {
            i += 1;
            continue;
        }
        let s = i;
        while i < b.len() && is_ident(b[i]) {
            i += 1;
        }
        let ident = &line[s..i];
        let Some(kind) = classify(ident) else { continue };
        let lower = ident.to_ascii_lowercase();

        let mut j = i;
        if j < b.len() && (b[j] == b'"' || b[j] == b'\'') {
            j += 1;
        }
        while j < b.len() && b[j] == b' ' {
            j += 1;
        }
        if j >= b.len() {
            continue;
        }
        match b[j] {
            b'=' | b':' => j += 1,
            // The Netscape cookie-jar layout puts the value after a tab.
            b'\t' if lower.contains("roblosecurity") => j += 1,
            _ => continue,
        }
        if b[j - 1] == b':' && line[j..].starts_with("//") {
            continue;
        }
        while j < b.len() && (b[j] == b' ' || b[j] == b'\t') {
            j += 1;
        }
        if j >= b.len() {
            continue;
        }

        let (vs, ve) = if b[j] == b'"' || b[j] == b'\'' {
            let q = b[j];
            let mut k = j + 1;
            while k < b.len() && b[k] != q {
                if b[k] == b'\\' {
                    k += 1;
                }
                k += 1;
            }
            (j + 1, k.min(b.len()))
        } else if lower.ends_with("cookie") || lower.ends_with("authorization") {
            // A header's value runs to the end of the line and has spaces and
            // semicolons in it.
            (j, b.len())
        } else {
            let plus_stops = lower == "gameinfo" || lower == "placelauncherurl";
            let mut k = j;
            while k < b.len()
                && !(b[k].is_ascii_whitespace()
                    || matches!(b[k], b';' | b',' | b'&' | b'"' | b'\'' | b'}' | b')' | b']' | b'>')
                    || (plus_stops && b[k] == b'+'))
            {
                k += 1;
            }
            (j, k)
        };
        if vs >= ve {
            continue;
        }
        let value = &line[vs..ve];
        // Already a placeholder, or a length like `<5 bytes, 5 chars>`.
        if value.starts_with('<') {
            i = ve;
            continue;
        }
        let lowered = value.to_ascii_lowercase();
        if matches!(lowered.as_str(), "null" | "none" | "nil" | "true" | "false" | "0") {
            i = ve;
            continue;
        }
        if kind == Kind::UserId && !value.bytes().all(|c| c.is_ascii_digit()) {
            i = ve;
            continue;
        }
        hits.push(Hit { start: vs, end: ve, kind });
        i = ve;
    }
    hits
}

/// Cut every URL's query, fragment and credentials.
fn strip_urls(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    loop {
        let Some(p) = rest.find("://") else {
            out.push_str(rest);
            return out;
        };
        let b = rest.as_bytes();
        let mut e = p + 3;
        while e < b.len() && !matches!(b[e], b' ' | b'\t' | b'"' | b'\'' | b'<' | b'>' | b')' | b']' | b'}' | b'|') {
            e += 1;
        }
        let url = &rest[p + 3..e];
        let auth_end = url.find(['/', '?', '#']).unwrap_or(url.len());
        let (auth, tail) = url.split_at(auth_end);
        let auth = auth.rsplit('@').next().unwrap_or(auth);
        let cut = tail.find(['?', '#']);
        let path = cut.map_or(tail, |k| &tail[..k]);
        out.push_str(&rest[..p + 3]);
        out.push_str(auth);
        out.push_str(path);
        match cut.map(|k| tail.as_bytes()[k]) {
            Some(b'?') => out.push_str("?<query removed>"),
            Some(_) => out.push_str("#<removed>"),
            None => {}
        }
        rest = &rest[e..];
    }
}

/// Replace every `_|WARNING:...` run, which is how a Roblox session cookie
/// begins wherever it turns up.
fn strip_warning_tokens(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(p) = rest.find("_|WARNING") {
        out.push_str(&rest[..p]);
        out.push_str("<redacted>");
        let tail = &rest[p..];
        let end = tail
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ';' | ','))
            .unwrap_or(tail.len());
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// Replace whole-word occurrences of `needle`. `boundary` says which bytes
/// count as part of a word and so forbid a match from touching them.
fn replace_bounded(text: &str, needle: &str, with: &str, boundary: fn(u8) -> bool) -> String {
    if needle.is_empty() || !text.contains(needle) {
        return text.to_string();
    }
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    let mut from = 0;
    while let Some(off) = text[from..].find(needle) {
        let s = from + off;
        let e = s + needle.len();
        let before_ok = s == 0 || !boundary(b[s - 1]);
        let after_ok = e >= b.len() || !boundary(b[e]);
        if before_ok && after_ok {
            out.push_str(&text[last..s]);
            out.push_str(with);
            last = e;
        }
        from = e;
    }
    out.push_str(&text[last..]);
    out
}

fn is_digit(b: u8) -> bool {
    b.is_ascii_digit()
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Digits after `/users/` or `/user/`, which carry an id with no key.
fn path_ids(line: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for marker in ["/users/", "/user/"] {
        let mut rest = line;
        while let Some(p) = rest.find(marker) {
            let after = &rest[p + marker.len()..];
            let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
            if digits.len() >= 5 {
                ids.push(digits);
            }
            rest = after;
        }
    }
    ids
}

/// Who is running Cordial, for the parts of a log that name the machine.
#[derive(Default, Clone, Debug)]
pub struct Identity {
    pub home: Option<String>,
    pub os_user: Option<String>,
    pub profiles: Vec<String>,
}

impl Identity {
    /// From the environment and the profile directory.
    pub fn here() -> Identity {
        Identity {
            home: std::env::var("HOME").ok().filter(|h| h.len() > 1),
            os_user: std::env::var("USER").or_else(|_| std::env::var("LOGNAME")).ok(),
            profiles: crate::profile::list(),
        }
    }
}

/// Redacts text, with placeholders that stay the same across everything it is
/// shown. See the module documentation for the rules.
#[derive(Default)]
pub struct Redactor {
    identity: Identity,
    ids: Vec<String>,
    names: Vec<String>,
}

impl Redactor {
    pub fn new(identity: Identity) -> Redactor {
        Redactor { identity, ids: Vec::new(), names: Vec::new() }
    }

    /// First pass: remember every id and name `text` reveals. Call on every
    /// source before redacting any of them.
    pub fn learn(&mut self, text: &str) {
        for line in text.lines() {
            if line.contains("[cookies]") || line.contains("[identity]") {
                continue;
            }
            for hit in keyed_hits(line) {
                let value = &line[hit.start..hit.end];
                match hit.kind {
                    Kind::UserId => self.note_id(value),
                    Kind::UserName => self.note_name(value),
                    _ => {}
                }
            }
            for id in path_ids(line) {
                self.note_id(&id);
            }
        }
    }

    fn note_id(&mut self, id: &str) {
        if id.len() >= 5 && !self.ids.iter().any(|i| i == id) {
            self.ids.push(id.to_string());
        }
    }

    fn note_name(&mut self, name: &str) {
        if name.len() >= 3
            && !NOT_A_NAME.contains(&name.to_ascii_lowercase().as_str())
            && !self.names.iter().any(|n| n == name)
        {
            self.names.push(name.to_string());
        }
    }

    fn id_placeholder(&self, id: &str) -> String {
        match self.ids.iter().position(|i| i == id) {
            Some(n) => format!("<user-id-{}>", n + 1),
            None => "<user-id>".to_string(),
        }
    }

    fn name_placeholder(&self, name: &str) -> String {
        match self.names.iter().position(|n| n == name) {
            Some(n) => format!("<username-{}>", n + 1),
            None => "<username>".to_string(),
        }
    }

    /// Redact a whole text, line by line.
    pub fn redact(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        for line in text.lines() {
            out.push_str(&self.redact_line(line));
            out.push('\n');
        }
        out
    }

    /// Redact one line.
    pub fn redact_line(&self, line: &str) -> String {
        if line.contains("[cookies]") || line.contains("[identity]") {
            return SESSION_MARKER.to_string();
        }
        if let Some(prefix) = chat_prefix(line) {
            return format!("{prefix} (message withheld)");
        }

        let mut s = line.to_string();
        for (n, id) in self.ids.iter().enumerate() {
            s = replace_bounded(&s, id, &format!("<user-id-{}>", n + 1), is_digit);
        }
        for (n, name) in self.names.iter().enumerate() {
            s = replace_bounded(&s, name, &format!("<username-{}>", n + 1), is_word);
        }
        if let Some(home) = &self.identity.home {
            s = s.replace(home.as_str(), "~");
        }
        if let Some(user) = &self.identity.os_user {
            if user.len() >= 3 && user != "root" {
                s = replace_bounded(&s, user, "<os-user>", is_word);
            }
        }
        s = replace_after(&s, "/home/", "<user>");
        s = replace_after(&s, "instances/", "<profile>");
        for profile in &self.identity.profiles {
            if profile.len() >= 4 && profile != "default" {
                s = replace_bounded(&s, profile, "<profile>", is_word);
            }
        }

        let hits = keyed_hits(&s);
        for hit in hits.iter().rev() {
            let value = s[hit.start..hit.end].to_string();
            let with = match hit.kind {
                Kind::Secret => "<redacted>".to_string(),
                Kind::UserId => self.id_placeholder(&value),
                Kind::UserName => self.name_placeholder(&value),
                Kind::Text => "<withheld>".to_string(),
            };
            s.replace_range(hit.start..hit.end, &with);
        }

        strip_warning_tokens(&strip_urls(&s))
    }
}

/// Replace the path segment after each `marker` with `with`, unless it already
/// is a placeholder or the stock `default` profile.
fn replace_after(text: &str, marker: &str, with: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(p) = rest.find(marker) {
        let (head, tail) = rest.split_at(p + marker.len());
        out.push_str(head);
        let end = tail
            .find(|c: char| c == '/' || c.is_whitespace() || matches!(c, '"' | '\'' | ':' | ',' | ')' | '>'))
            .unwrap_or(tail.len());
        let segment = &tail[..end];
        if segment.is_empty() || segment.starts_with('<') || segment.starts_with('~') || segment == "default" {
            out.push_str(segment);
        } else {
            out.push_str(with);
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// `[FLog::...]` for a channel that carries what people said, or `None`.
fn chat_prefix(line: &str) -> Option<&str> {
    let p = line.find("[FLog::")?;
    let close = p + line[p..].find(']')?;
    let channel = line[p + 7..close].to_ascii_lowercase();
    channel.contains("chat").then(|| &line[..=close])
}

/// An engine log chosen for the archive.
#[derive(Debug, Clone)]
pub struct EngineLog {
    pub label: &'static str,
    pub name: String,
    pub text: String,
    pub cut: bool,
}

/// The newest engine logs in `dir`: `*_last.log` first, and any `*.log` if the
/// engine has not rotated one yet.
pub fn newest_engine_logs(dir: &Path) -> Vec<PathBuf> {
    let Ok(read) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut all: Vec<(std::time::SystemTime, PathBuf, bool)> = read
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let name = path.file_name()?.to_str()?.to_string();
            if !name.ends_with(".log") {
                return None;
            }
            let modified = e.metadata().ok()?.modified().ok()?;
            Some((modified, path, name.ends_with("_last.log")))
        })
        .collect();
    let any_last = all.iter().any(|(_, _, last)| *last);
    if any_last {
        all.retain(|(_, _, last)| *last);
    }
    all.sort_by(|a, b| b.0.cmp(&a.0));
    all.into_iter().take(ENGINE_LOGS).map(|(_, p, _)| p).collect()
}

/// The last [`ENGINE_LOG_BYTES`] of a file, from a line boundary.
fn read_tail(path: &Path) -> std::io::Result<(String, bool)> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let cut = len > ENGINE_LOG_BYTES;
    if cut {
        file.seek(SeekFrom::Start(len - ENGINE_LOG_BYTES))?;
    }
    let mut bytes = Vec::new();
    file.take(ENGINE_LOG_BYTES).read_to_end(&mut bytes)?;
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if cut {
        if let Some(nl) = text.find('\n') {
            text.drain(..=nl);
        }
    }
    Ok((text, cut))
}

/// Everything that goes in the archive, before redaction.
#[derive(Default)]
pub struct Inputs {
    pub diagnostics: String,
    pub launcher: Vec<String>,
    pub client: Option<(Vec<String>, String)>,
    pub health: Vec<String>,
    pub engine: Vec<EngineLog>,
    pub coredump: String,
    pub identity: Identity,
}

/// One file in the archive.
pub struct Entry {
    pub name: String,
    pub data: Vec<u8>,
}

fn safe_name(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '_' }).collect()
}

/// Redact `inputs` and lay out the archive's files.
pub fn entries(inputs: &Inputs) -> Vec<Entry> {
    let mut redactor = Redactor::new(inputs.identity.clone());
    redactor.learn(&inputs.diagnostics);
    redactor.learn(&inputs.launcher.join("\n"));
    if let Some((lines, command)) = &inputs.client {
        redactor.learn(&lines.join("\n"));
        redactor.learn(command);
    }
    redactor.learn(&inputs.health.join("\n"));
    for log in &inputs.engine {
        redactor.learn(&log.text);
    }

    let mut out = Vec::new();
    let mut add = |name: &str, text: String| out.push(Entry { name: name.to_string(), data: text.into_bytes() });

    let mut listed = vec![("diagnostics.txt", "the Report a Problem block, with the machine checks")];
    add("diagnostics.txt", redactor.redact(&inputs.diagnostics));

    if inputs.launcher.is_empty() {
        add("launcher.log", "The launcher printed nothing this session, or log capture was switched off.\n".into());
    } else {
        add("launcher.log", redactor.redact(&inputs.launcher.join("\n")));
    }
    listed.push(("launcher.log", "everything the launcher printed this session, which includes the client's output"));

    match &inputs.client {
        Some((lines, command)) => {
            let body = format!(
                "{}\nIt was started with:\n{}\n",
                redactor.redact(&lines.join("\n")).trim_end(),
                redactor.redact_line(command)
            );
            add("client-output.log", body);
        }
        None => add("client-output.log", "No client was started this session.\n".into()),
    }
    listed.push(("client-output.log", "the last 200 lines of the most recent client, and its command line"));

    if inputs.health.is_empty() {
        add("health.log", "No `[cordial] health:` line was seen this session.\n".into());
    } else {
        add("health.log", redactor.redact(&inputs.health.join("\n")));
    }
    listed.push(("health.log", "the last `[cordial] health:` lines"));

    let mut engine_listed = Vec::new();
    for log in &inputs.engine {
        let name = format!("engine/{}-{}", log.label, safe_name(&log.name));
        let note = if log.cut { "(the start of this log was cut to its last 2 MiB)\n" } else { "" };
        add(&name, format!("{note}{}", redactor.redact(&log.text)));
        engine_listed.push(name);
    }

    add("coredump.txt", redactor.redact(&inputs.coredump));
    listed.push(("coredump.txt", "whether a core dump exists, and the command to read it; no core file is included"));

    let mut readme = format!(
        "Cordial log archive, made by {}.\n\nFiles:\n",
        crate::version::full()
    );
    for (name, what) in &listed {
        readme.push_str(&format!("  {name}  {what}\n"));
    }
    if engine_listed.is_empty() {
        readme.push_str("  engine/  no Roblox engine log was found for this profile\n");
    }
    for name in &engine_listed {
        readme.push_str(&format!("  {name}  the engine's own log, newest first\n"));
    }
    readme.push_str(
        "\nRedacted before writing: cookie, ticket, token, password and authorization values; \
         every URL's query string; user ids and usernames (replaced by numbered placeholders, the \
         same throughout this archive); chat channels; typed text; your home directory and \
         profile names.\n\
         The machine's hostname is kept in diagnostics.txt, as it is when you press Copy. \
         That is pattern matching, so read the files before you post them if the account \
         matters to you. Core files are never included.\n",
    );
    out.insert(0, Entry { name: "README.txt".into(), data: readme.into_bytes() });
    out
}

/// The archive as `.zip` bytes.
pub fn zip_bytes(entries: &[Entry]) -> Result<Vec<u8>, String> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for entry in entries {
        writer.start_file(entry.name.as_str(), options).map_err(|e| e.to_string())?;
        writer.write_all(&entry.data).map_err(|e| e.to_string())?;
    }
    writer.finish().map(|c| c.into_inner()).map_err(|e| e.to_string())
}

/// What `coredumpctl list cordial-run` said, as the text of `coredump.txt`.
pub fn coredump_text(listing: Result<String, String>) -> String {
    const HOW: &str = "\nTo list them:   coredumpctl list cordial-run\n\
                       To read one:    coredumpctl info cordial-run\n\
                       To save one:    coredumpctl dump cordial-run -o cordial.core\n\
                       A core file holds the client's memory, session included, so this archive never \
                       carries one. Keep it, and say so in the issue; a maintainer will say how to share it.\n";
    match listing {
        Err(why) => format!("Cordial could not check for a core dump: {why}.\nIf the client crashed, run this on the host:\n{HOW}"),
        Ok(text) => {
            let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
            match lines.last() {
                None => format!("systemd-coredump has no core dump of cordial-run.\n{HOW}"),
                Some(newest) => format!(
                    "systemd-coredump has {} core dump(s) of cordial-run. Newest:\n  {newest}\n{HOW}",
                    lines.len()
                ),
            }
        }
    }
}

fn coredump_listing() -> Result<String, String> {
    if Path::new("/.flatpak-info").exists() {
        return Err("Cordial is running inside the Flatpak sandbox, which cannot see the host's coredumpctl".into());
    }
    let out = std::process::Command::new("coredumpctl")
        .args(["list", "--no-pager", "--no-legend", "-q", "cordial-run"])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("coredumpctl did not run ({e})"))?;
    // Exit 1 with nothing on stdout is how it says there are none.
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Read everything the archive holds from this session and the profile.
pub fn gather(profile: &str, diagnostics: String) -> Inputs {
    let mut engine = Vec::new();
    if let Ok(dir) = crate::profile::dir(profile) {
        for (label, build) in [("phone", Build::Phone), ("quest", Build::Quest)] {
            let logs = crate::freeze_recovery::logs_dir(&dir, build);
            for path in newest_engine_logs(&logs) {
                let Ok((text, cut)) = read_tail(&path) else { continue };
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("engine.log").to_string();
                engine.push(EngineLog { label, name, text, cut });
            }
        }
    }
    Inputs {
        diagnostics,
        launcher: crate::session_log::launcher_lines(),
        client: crate::session_log::last_client(),
        health: crate::session_log::health_lines(),
        engine,
        coredump: coredump_text(coredump_listing()),
        identity: Identity::here(),
    }
}

/// The finished `.zip` for `profile`. Blocks on a subprocess and on file
/// reads, so call it from a worker.
pub fn export(profile: &str, diagnostics: String) -> Result<Vec<u8>, String> {
    zip_bytes(&entries(&gather(profile, diagnostics)))
}

#[cfg(test)]
mod tests;
