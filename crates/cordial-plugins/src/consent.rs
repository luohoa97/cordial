//! What the user is asked at install, and when they are asked nothing.
//!
//! [ADR-021](../../../docs/adr/ADR-021-everything-is-a-plugin.md). Everything
//! is a plugin; some plugins happen to contain code. Whether one does is a
//! property read off its manifest — an `entry`, or capabilities, or both — and
//! it is what decides whether there is anything to ask about.
//!
//! Three rules, and the wording of the third is most of the work.
//!
//! **A plugin with no code gets no prompt at all.** No entry module, nothing to
//! run; no capabilities, nothing it could reach. A texture pack installs
//! silently, the same way copying a file into a folder does. This is not
//! leniency: if every import prompts, the prompt means nothing by the third one
//! and the user has been trained to dismiss the one that mattered.
//!
//! **Code starts disabled regardless.** The prompt is not the gate; the toggle
//! is. Consent and enablement are separate acts, which is the argument
//! [`crate::enablement`] already makes in the other direction — that disabling
//! must not cost the approvals. Run forwards, the same separation means
//! approving what a plugin *may* do does not start it doing it.
//!
//! **Every capability is spelled out as an effect.** The prompt lists what the
//! plugin will be able to do, in sentences a person can judge, never
//! `flags.write, presence.set` and never "this plugin wants to run code, allow?"
//! The sentences live on [`Capability::consequence`] beside the enum, so a
//! capability cannot be added without one.

use crate::capability::Capability;
use crate::manifest::Plugin;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Why a plugin needs asking about, or the fact that it does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing to run and nothing it could reach. Install it and say so; do
    /// not interrupt.
    Silent,
    /// There is code, or there are capabilities, or both. Show [`Prompt`].
    Ask(Prompt),
}

/// What the install dialog says.
///
/// Deliberately not a formatted string: the shell draws this with its own
/// widgets and its own escaping, the same division ADR-020 draws for a
/// plugin's preferences page. A manifest supplies a name and a list of
/// capability *names*; every sentence the user reads comes from Cordial.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    /// The plugin's display name, falling back to its id.
    pub name: String,
    pub id: String,
    /// Whether this plugin has an entry module. Governs the closing line,
    /// because "it will start switched off" is only true of something that
    /// could start at all.
    pub has_code: bool,
    /// One line per capability: the capability, and what granting it lets the
    /// plugin do. In the enum's own order, so two plugins asking for the same
    /// things list them the same way.
    pub effects: Vec<Effect>,
}

/// One capability, as a person reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Effect {
    pub capability: Capability,
    pub description: &'static str,
}

impl Prompt {
    /// The heading. Says what the plugin will be *able* to do, not what it
    /// wants — "wants" invites the reader to grant it out of politeness.
    pub fn heading(&self) -> String {
        if self.effects.is_empty() {
            format!("{} runs code, and asks for no permissions.", self.name)
        } else {
            format!("{} will be able to:", self.name)
        }
    }

    /// The closing line, which is the one that stops the dialog reading as a
    /// gate. A user who clicks through has still not started anything.
    pub fn footer(&self) -> &'static str {
        if self.has_code {
            "It starts switched off. Turn it on from the Plugins page when you want it to run. \
             You can change any of these permissions there too, at any time."
        } else {
            "You can change any of these permissions from the Plugins page at any time."
        }
    }
}

/// Whether installing `plugin` should ask, and what it should say.
///
/// The gate is "contains code" read off the manifest, never which import
/// button was pressed — there is one import path precisely so that a texture
/// pack and an integration cannot be told apart by how they arrived.
pub fn verdict(plugin: &Plugin) -> Verdict {
    if !plugin.has_code() && plugin.requested.is_empty() {
        return Verdict::Silent;
    }
    let name = if plugin.manifest.name.is_empty() {
        plugin.manifest.id.clone()
    } else {
        plugin.manifest.name.clone()
    };
    Verdict::Ask(Prompt {
        name,
        id: plugin.manifest.id.clone(),
        has_code: plugin.has_code(),
        // `requested` is a `BTreeSet<Capability>`, so this is the enum's own
        // order and not the manifest's. Two plugins asking for the same things
        // then read the same way, and a manifest cannot put the alarming one
        // last by writing it last.
        effects: plugin
            .requested
            .iter()
            .map(|c| Effect { capability: *c, description: c.consequence() })
            .collect(),
    })
}

/// Whether a freshly installed plugin should be written into
/// `plugin-enabled.json` as off.
///
/// **A plugin with code starts disabled, whatever the user said to the
/// prompt.** [`crate::enablement`]'s "absence means enabled" is right for a
/// plugin nobody has had an opinion about, and wrong for one that has just
/// arrived with code in it: absence would mean an install dialog's OK button
/// both granted the capabilities and started the process, which is one act
/// where there should be two.
///
/// Data-only plugins are left absent, and therefore on. There is nothing to
/// start, and an asset pack that had to be switched on after installing would
/// be a switch with no argument behind it.
pub fn starts_disabled(plugin: &Plugin) -> bool {
    plugin.has_code()
}

/// Which built-in plugins a profile has already been asked about, so the
/// question is not asked again on every visit to the Plugins page.
///
/// **Why a built-in needs this and a user install does not.** Installing a
/// plugin is itself a single, one-off event — the moment the archive is
/// unpacked is the one and only moment to ask, and nothing has to be
/// remembered afterwards because the question is never asked a second time
/// from that code path. A built-in has no such moment: it arrives with
/// Cordial itself and is simply *present* the first time a profile's Plugins
/// page is built, possibly a profile created long before this file existed.
/// "Have we asked" therefore has to be a fact recorded somewhere both able to
/// outlive one Settings session and scoped the same way grants and enablement
/// already are — per profile, so a built-in asked about in one profile is
/// asked about again in another, the same isolation ADR-013 gives everything
/// else here.
///
/// Recording *that* a plugin was asked, not *what was answered* — declining
/// is a legitimate, complete answer and must not be treated as "still
/// pending". What was granted lives in `grants`, exactly as it does for a
/// user install; this file only ever says whether the question has been put.
pub fn seen_path_in(profile_dir: &Path) -> PathBuf {
    profile_dir.join("plugin-consent-seen.json")
}

fn load_seen(path: &Path) -> BTreeSet<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return BTreeSet::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// Whether `id` has already been asked about in this profile.
///
/// A missing or malformed file reads as "nobody has been asked", the same
/// fail-safe direction `enablement::load` takes for its own file — the
/// consequence of getting this wrong is an extra prompt somebody dismisses
/// once more, not a capability granted nobody agreed to, so erring toward
/// asking again is the harmless side of the two ways this could fail.
pub fn has_been_asked(path: &Path, id: &str) -> bool {
    load_seen(path).contains(id)
}

/// Record that `id` has been asked about, whichever way it was answered.
pub fn mark_asked(path: &Path, id: &str) -> std::io::Result<()> {
    let mut seen = load_seen(path);
    if !seen.insert(id.to_string()) {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(&seen)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = path.with_extension("json.new");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest;
    use std::path::PathBuf;

    fn parse(json: &str) -> Plugin {
        manifest::parse(json, &PathBuf::from("/plugins/example")).unwrap()
    }

    #[test]
    fn a_plugin_that_is_only_data_is_installed_without_a_prompt() {
        // The rule that makes the other prompts mean something. A texture pack
        // has no entry module and no capabilities, so there is nothing to run
        // and nothing it could reach — asking anyway is what makes the third
        // prompt reflexive.
        let pack = parse(r#"{"id":"retro-ui","name":"Retro UI"}"#);
        assert!(!pack.has_code());
        assert_eq!(verdict(&pack), Verdict::Silent);
    }

    #[test]
    fn a_plugin_with_code_is_asked_about_even_with_no_capabilities() {
        // Nothing brokered is still something running. It gets the shorter
        // dialog rather than none, because "starts switched off" is a fact the
        // user needs and there is nowhere else to put it.
        let p = parse(r#"{"id":"quiet","name":"Quiet","entry":"main.ts"}"#);
        let Verdict::Ask(prompt) = verdict(&p) else { panic!("code must be asked about") };
        assert!(prompt.effects.is_empty());
        assert!(prompt.heading().contains("asks for no permissions"));
        assert!(prompt.footer().contains("starts switched off"));
    }

    #[test]
    fn data_that_asks_for_a_capability_is_still_asked_about() {
        // The gate is "contains code" in the sense of "could do something",
        // not "has an entry module". A manifest with capabilities and no entry
        // is a contradiction a hostile author might try, and the safe reading
        // of it is the one that asks.
        let p = parse(r#"{"id":"odd","capabilities":["presence.set"]}"#);
        assert!(!p.has_code());
        let Verdict::Ask(prompt) = verdict(&p) else { panic!("a capability must be asked about") };
        assert_eq!(prompt.effects.len(), 1);
        // Nothing to start, so the footer must not promise it starts off.
        assert!(!prompt.footer().contains("starts switched off"));
    }

    #[test]
    fn the_prompt_lists_effects_and_never_capability_names() {
        // "This plugin wants flags.write, allow?" is the failure this whole
        // module exists to avoid: it is accurate, unreadable, and answered yes
        // by everybody.
        let p = parse(
            r#"{"id":"tweaks","name":"FPS Tweaks","entry":"main.ts",
                "capabilities":["flags.write","presence.set"]}"#,
        );
        let Verdict::Ask(prompt) = verdict(&p) else { panic!() };
        assert_eq!(prompt.effects.len(), 2);
        for effect in &prompt.effects {
            assert!(!effect.description.is_empty());
            assert!(
                !effect.description.contains(effect.capability.name()),
                "the wire name must not be what the user reads: {:?}",
                effect.description
            );
        }
    }

    #[test]
    fn flags_write_is_described_as_changing_cordial_itself() {
        // ADR-020's consequence, carried into the one place it is read. A
        // prompt saying "change some Roblox settings" would be technically
        // true and materially misleading, because the same capability sets
        // the graphics backend and the present mode.
        let p = parse(r#"{"id":"t","entry":"m.ts","capabilities":["flags.write"]}"#);
        let Verdict::Ask(prompt) = verdict(&p) else { panic!() };
        let text = prompt.effects[0].description;
        assert!(text.contains("Cordial"), "{text:?}");
        assert!(text.contains("graphics backend") && text.contains("present mode"), "{text:?}");
    }

    #[test]
    fn effects_are_listed_in_the_enums_order_not_the_manifests() {
        // A manifest that could choose the order could bury the alarming
        // permission under two harmless ones, or list them differently from
        // the next plugin so nothing is recognisable at a glance.
        let a = parse(r#"{"id":"a","entry":"m.ts","capabilities":["presence.set","flags.read","log"]}"#);
        let b = parse(r#"{"id":"b","entry":"m.ts","capabilities":["log","flags.read","presence.set"]}"#);
        let (Verdict::Ask(pa), Verdict::Ask(pb)) = (verdict(&a), verdict(&b)) else { panic!() };
        let names = |p: &Prompt| p.effects.iter().map(|e| e.capability.name()).collect::<Vec<_>>();
        assert_eq!(names(&pa), names(&pb));
        assert_eq!(names(&pa), vec!["flags.read", "log", "presence.set"]);
    }

    #[test]
    fn code_starts_disabled_and_data_does_not() {
        // Consent and enablement are separate acts. An install dialog's OK
        // must not both grant the capabilities and start the process.
        let code = parse(r#"{"id":"c","entry":"m.ts","capabilities":["log"]}"#);
        let data = parse(r#"{"id":"d","name":"Textures"}"#);
        assert!(starts_disabled(&code));
        assert!(!starts_disabled(&data), "there is nothing to start, so a switch would have no argument");
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cordial-consent-seen-test-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn nobody_has_been_asked_before_the_file_exists() {
        let dir = scratch("fresh");
        assert!(!has_been_asked(&seen_path_in(&dir), "discord-presence"));
    }

    #[test]
    fn marking_a_plugin_asked_makes_it_stick() {
        let dir = scratch("mark");
        let path = seen_path_in(&dir);
        mark_asked(&path, "discord-presence").unwrap();
        assert!(has_been_asked(&path, "discord-presence"));
        assert!(!has_been_asked(&path, "some-other-plugin"), "one plugin's record must not answer for another");
    }

    #[test]
    fn marking_the_same_plugin_twice_is_harmless() {
        let dir = scratch("idempotent");
        let path = seen_path_in(&dir);
        mark_asked(&path, "discord-presence").unwrap();
        mark_asked(&path, "discord-presence").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("discord-presence").count(), 1, "{text}");
    }

    #[test]
    fn a_malformed_file_reads_as_nobody_asked_rather_than_erroring() {
        // Erring the other way -- treating an unreadable file as "everybody
        // has been asked" -- would suppress the one prompt this module
        // exists to show, permanently and silently, for a file a stray edit
        // or a half-written disk could produce.
        let dir = scratch("malformed");
        let path = seen_path_in(&dir);
        std::fs::write(&path, "{not json").unwrap();
        assert!(!has_been_asked(&path, "discord-presence"));
    }
}
