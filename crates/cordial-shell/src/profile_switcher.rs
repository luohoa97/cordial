//! Choosing the profile the next instance runs.
//!
//! A menu button at the start of the header bar, labelled with the profile the
//! next launch will use. Its popover lists [`profile::list`] as radio items and
//! ends with "New Profile…" and "Delete Profile…". ADR-012 makes a profile a
//! directory and an instance a window; this is where one is picked for the
//! other.
//!
//! **Why the header bar, reversing this file's earlier answer (2026-10-08).**
//! The first version was an `AdwAvatar` in the top right, and the second an
//! `AdwComboRow` above the Launch button with a create and a delete button
//! beside it. The avatar was dropped as a browser convention, and the row
//! replaced it on the argument that the profile is a launch parameter and so
//! belongs beside the button it governs. That argument holds, and the row
//! still cost more than it seemed to: a titled row, a subtitle and two icon
//! buttons sitting over the one button the window exists for made the launcher
//! read as a form with a button at the bottom. What the avatar got wrong was
//! its *shape* and its far corner, not the header bar. A labelled menu button at the start says "this is the
//! context the window is acting in" in words, takes no space from the body,
//! and keeps the two actions that are not values out of the way of the list.
//! GNOME's HIG still has no page for a profile switcher, so this follows its
//! general header bar guidance and is `INFERRED` to be the right call rather
//! than measured to be one.
//!
//! What the old row's subtitle said -- an uncreated profile, one held by another
//! window, a pin from ADR-033 -- is a caption under the Launch button, empty and
//! hidden in the ordinary case, so none of it was lost with the row.
//!
//! **Why it is in the shell and not in a client.** A running client cannot
//! change profile: `cordial_runtime::profile::set_active` refuses a second,
//! different directory outright — "a profile cannot be changed while the client
//! is up" — the `flock` is held for the lifetime of that process, and the
//! engine's storage root is resolved before the first frame. A switcher in the
//! engine's window would be a control that cannot do what it looks like it does,
//! which is the interface version of the stub that reports success AGENTS.md
//! rules out. Here it decides what the *next* launch runs, and running a second
//! profile beside the first is the same gesture: pick another and press Roblox,
//! which is all "two accounts at once" has ever been.
//!
//! There used to be a text entry for this in Settings and it is gone rather than
//! kept beside this. Two ways to set one value drift, and the one that drifts is
//! the one nobody is looking at.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use libadwaita as adw;
use libadwaita::gtk;
use libadwaita::gtk::{gio, glib};
use libadwaita::prelude::*;

use crate::settings::persist;
use crate::shell_config::ShellConfig;
use cordial_shell::profile;

/// How wide a profile name is allowed to make the row, in characters.
/// `profile::is_valid_name` allows 64, which is far past what fits;
/// `fdsafdsagfdsgfdgfdgfd` is on this developer's disk right now and without a
/// cap a `GtkLabel` asks for its whole natural width and stretches the window.
const NAME_WIDTH: i32 = 24;

/// Whether a profile can be handed to a new instance.
///
/// Answered by taking ADR-012's claim and dropping it again, rather than by a
/// liveness check of this module's own. The `flock` is the only thing that
/// actually decides, so a second opinion — a PID file, a scan of `/proc` — could
/// disagree with it, and the disagreement the user would meet is the worst
/// direction: an entry offered as free, chosen, and then refused by `try_launch`
/// for a reason the list had just said did not apply.
///
/// The cost is honest and small. The probe really does hold each profile's lock
/// for as long as it takes to release it, so a launch racing this list being
/// drawn could be refused when it would otherwise have been allowed. That is the
/// same refusal a second launch produces, it names the profile, and trying again
/// succeeds — a better failure than marks that are guesswork.
#[derive(Debug, PartialEq, Eq)]
pub enum Availability {
    Free,
    /// Held by another instance. Not a fault: it is the lock doing its job.
    Running,
    /// The directory is there and cannot be used — permissions, most likely.
    /// Kept apart from `Running` because the answer to it is completely
    /// different, and `profile::Error` already draws that line for the same
    /// reason.
    Unusable(String),
}

fn availability(name: &str) -> Availability {
    match profile::acquire(name) {
        Ok(claim) => {
            // Released immediately and explicitly. The launcher hands its claim
            // to the client it spawns; this one belongs to nobody, and holding
            // it a line longer than needed would mean the chooser itself was the
            // instance keeping a profile busy.
            drop(claim);
            Availability::Free
        }
        Err(profile::Error::Busy(..)) => Availability::Running,
        Err(profile::Error::Unusable(message)) => Availability::Unusable(message),
    }
}

/// What the create dialog makes of what has been typed so far.
#[derive(Debug, PartialEq, Eq)]
pub enum NameCheck {
    /// Nothing typed yet. No complaint to make, and nothing to create either.
    Empty,
    /// [`profile::dir`]'s own refusal, verbatim.
    ///
    /// Quoted rather than reworded so that the sentence a user meets when they
    /// type a slash here is the same sentence they would meet anywhere else the
    /// name is resolved. Refused rather than sanitised, which is `profile`'s
    /// decision and not this module's: silently rewriting a name would mean the
    /// profile someone asked for is not the one they get.
    Invalid(String),
    /// A profile by that name is already there, so "create" is really "switch
    /// to". Said out loud rather than left to look like a no-op.
    Existing,
    New,
}

pub fn check_name(name: &str, existing: &[String]) -> NameCheck {
    if name.is_empty() {
        return NameCheck::Empty;
    }
    if let Err(message) = profile::dir(name) {
        return NameCheck::Invalid(message);
    }
    if existing.iter().any(|e| e == name) {
        NameCheck::Existing
    } else {
        NameCheck::New
    }
}

/// The names the row offers: exactly the profiles that exist, and deliberately
/// nothing else.
///
/// This is a thin wrapper on purpose. An earlier version also synthesised the
/// chosen profile when that had never been launched and so had no directory yet,
/// on the argument that a list which omits the selected entry looks broken. That
/// was wrong, and it is the kind of wrong this project cares about: a profile
/// *is* a signed-in session, so a launcher that lists one which does not exist
/// is claiming an account that was never there. Nothing here seeds, suggests or
/// pre-creates a profile, and the test below is what keeps it that way.
fn offered() -> Vec<String> {
    profile::list()
}

/// What the caption under the Launch button says, given what is known about the
/// profile.
///
/// **The ordinary case says nothing, and that is the change.** This used to read
/// "One account's Roblox storage, held by one window at a time" whenever the
/// selected profile was free — which is ADR-012's definition of the word rather
/// than anything the person choosing needs. It was true of every entry in the
/// list, so it distinguished nothing; it taught the data model to somebody who
/// only wanted to press play; and a permanent two-line subtitle over a permanent
/// group header is what made a launcher read as a settings page. An empty
/// caption is hidden outright, which has the useful side effect that a line
/// appearing at all is the signal that something is worth reading.
///
/// The three that remain are each a fact about *this* profile that changes what
/// pressing the button will do. `None` is the profile with no directory yet:
/// deliberately not probed, because [`profile::acquire`] creates the directory
/// on its way to the lock, so asking whether an uncreated profile is free would
/// create it.
fn subtitle(name: &str, availability: Option<&Availability>) -> String {
    match availability {
        None => format!("{name} will be created when you launch"),
        Some(Availability::Free) => String::new(),
        // "Opened", not "Open". The imperative reading of "Open in another
        // window" is an instruction -- press this to open it over there -- and
        // the line is describing a state, not offering an action. What pressing
        // Roblox will do is not its job: the button stays live on purpose, and
        // the dialog it raises offers to close the other client.
        Some(Availability::Running) => "Opened in another window".into(),
        Some(Availability::Unusable(message)) => message.clone(),
    }
}

/// Longest a profile name or an operating-system message is drawn in a menu
/// item before it is shortened in the middle. See [`shorten`].
const MENU_TEXT_CHARS: usize = 40;

/// `text` cut to `max` characters with an ellipsis in the middle, so the end of
/// a long name -- usually the part that tells two of them apart -- survives.
///
/// A menu item does not ellipsise on its own: the popover grows to the widest
/// label, and `profile::is_valid_name` allows 64 characters.
fn shorten(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        return text.to_string();
    }
    let keep = max.saturating_sub(1);
    let tail = keep / 2;
    let head = keep - tail;
    let mut out: String = chars[..head].iter().collect();
    out.push('…');
    out.extend(chars[chars.len() - tail..].iter());
    out
}

/// A string as a menu item's label, which reads `_` as a mnemonic marker.
/// `profile::is_valid_name` allows underscores, so `alt_account` would otherwise
/// be drawn as "altaccount" with the `a` underlined.
fn menu_label(text: &str) -> String {
    text.replace('_', "__")
}

/// What a profile's menu item says, and whether choosing it is allowed.
///
/// A busy profile stays choosable and says so, for the reason it always has:
/// pressing Roblox on it raises the dialog that offers to close the client
/// holding it, which is the only way to clear a stray client from inside
/// Cordial. An unusable one is not, because nothing can fix it from here.
fn item_text(name: &str, availability: &Availability) -> (String, bool) {
    let shown = shorten(name, MENU_TEXT_CHARS);
    match availability {
        Availability::Free => (shown, true),
        Availability::Running => (format!("{shown} (opened in another window)"), true),
        Availability::Unusable(message) => {
            (format!("{shown} (unusable: {})", shorten(message, MENU_TEXT_CHARS)), false)
        }
    }
}

/// The config, the file it is saved to, and the widgets showing the answer.
#[derive(Clone)]
struct Switcher {
    config: Rc<RefCell<ShellConfig>>,
    config_path: Rc<PathBuf>,
    /// The header bar button. Its label is the profile the next launch uses,
    /// which is the configured name even when no such directory exists yet.
    button: gtk::MenuButton,
    label: gtk::Label,
    /// The menu the button opens, rewritten each time it is about to open.
    menu: gio::Menu,
    /// Radio state for the list: the string state is the chosen profile.
    choose: gio::SimpleAction,
    /// Disabled when there is no profile on disk to delete, which is the state
    /// of a fresh install.
    delete: gio::SimpleAction,
    /// Under the Launch button. Empty, and hidden, in the ordinary case.
    note: gtk::Label,
}

impl Switcher {
    fn current(&self) -> String {
        self.config.borrow().profile.clone()
    }

    /// Persisted, not only shown. `ShellConfig.profile` is what `try_launch`
    /// reads, so a choice this window forgets is a choice that never happened.
    fn choose(&self, name: &str) {
        self.config.borrow_mut().profile = name.to_string();
        persist(&self.config, &self.config_path);
        self.describe();
    }

    /// Re-read the disk and redraw. Called when the button is built, after a
    /// profile is created or deleted, when Settings closes, and as the menu
    /// opens, rather than held as state: a profile can appear or disappear
    /// from under this window at any time, and a list assembled once is
    /// confidently wrong by the second launch.
    fn refresh(&self) {
        self.rebuild_menu();
        self.describe();
    }

    /// The list, as it is right now. Availability is asked here, as the menu
    /// opens, so what it shows is what was true when it was opened.
    fn rebuild_menu(&self) {
        self.menu.remove_all();

        let names = offered();
        let profiles = gio::Menu::new();
        if names.is_empty() {
            // A disabled item rather than an empty section, which would draw a
            // menu that opens onto nothing but its own two actions.
            let none = gio::MenuItem::new(Some("No Profiles Yet"), Some("profile.none"));
            profiles.append_item(&none);
        }
        for name in &names {
            let (text, choosable) = item_text(name, &availability(name));
            let item = gio::MenuItem::new(Some(&menu_label(&text)), None);
            if choosable {
                item.set_action_and_target_value(Some("profile.choose"), Some(&name.to_variant()));
            } else {
                item.set_action_and_target_value(Some("profile.unavailable"), None);
            }
            profiles.append_item(&item);
        }
        self.menu.append_section(None, &profiles);

        let actions = gio::Menu::new();
        actions.append(Some("_New Profile…"), Some("profile.new"));
        actions.append(Some("_Delete Profile…"), Some("profile.delete"));
        self.menu.append_section(None, &actions);
    }

    /// Say what the launch will actually do. The wording, and why the ordinary
    /// case says nothing at all, is on [`subtitle`].
    fn describe(&self) {
        let name = self.current();
        // Probed only once the profile is known to exist, and that order is not
        // incidental: `profile::acquire` creates the directory on its way to the
        // lock, so asking whether a not-yet-created profile is free would create
        // it — a launcher conjuring an account out of drawing its own caption.
        let exists = offered().contains(&name);
        self.delete.set_enabled(exists);
        self.choose.set_state(&name.to_variant());
        let availability = exists.then(|| availability(&name));
        let pin = profile::dir(&name).ok().and_then(|d| profile::pinned_version(&d));
        let note = with_pin(subtitle(&name, availability.as_ref()), pin.as_deref());
        self.note.set_text(&note);
        self.note.set_visible(!note.is_empty());

        self.label.set_text(&name);
        // The visible label is the bare name, which says nothing about what it
        // names to somebody who cannot see where the button sits.
        self.button.update_property(&[gtk::accessible::Property::Label(&format!("Profile: {name}"))]);
    }
}

/// ADR-033 says a pinned profile "says so in the launcher", and the caption under
/// the Launch button is the launcher's only line about the profile. A pin is the
/// one setting that stops Roblox updates reaching somebody, so it is shown even
/// in the ordinary case that otherwise says nothing.
fn with_pin(base: String, pin: Option<&str>) -> String {
    match (pin, base.is_empty()) {
        (None, _) => base,
        (Some(v), true) => format!("Pinned to Roblox {v}"),
        (Some(v), false) => format!("{base}. Pinned to Roblox {v}"),
    }
}

/// The switcher: a button for the header bar and a caption for under the Launch
/// button, which share one state and are refreshed together.
///
/// **The button is handed back rather than kept private**, because `win.profile`
/// has to be able to open the menu. That action used to call `grab_focus()` on
/// the group around the old row, which focuses something that is not the
/// control, changes nothing anybody can see, and made "Choose a Profile" on the
/// profile-busy dialog indistinguishable from "Cancel" -- reported on 2026-08-28
/// as the two buttons doing the same thing, which from the outside they did.
pub struct Chooser {
    pub button: gtk::MenuButton,
    pub note: gtk::Label,
    /// Re-read what both say. Settings can pin the profile shown here, and
    /// nothing else would tell them until the profile was switched.
    pub refresh: Rc<dyn Fn()>,
}

pub fn build(config: Rc<RefCell<ShellConfig>>, config_path: Rc<PathBuf>) -> Chooser {
    let label = gtk::Label::new(None);
    label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    // Ellipsising alone does nothing: a `GtkLabel` still asks for its whole
    // natural width and the header bar grows to give it. Capping the character
    // count is what bounds that request.
    label.set_max_width_chars(NAME_WIDTH);

    let menu = gio::Menu::new();
    // `always_show_arrow` because a custom child gets none of its own, and the
    // caret is what tells somebody a name in a header bar is a menu.
    let button = gtk::MenuButton::builder()
        .child(&label)
        .always_show_arrow(true)
        .tooltip_text("Switch Profile")
        .menu_model(&menu)
        .build();

    let current = config.borrow().profile.clone();
    let choose = gio::SimpleAction::new_stateful("choose", Some(glib::VariantTy::STRING), &current.to_variant());
    let new = gio::SimpleAction::new("new", None);
    let delete = gio::SimpleAction::new("delete", None);
    // Present so the menu can draw an item for a profile that cannot be chosen,
    // and never enabled, which is what makes GTK grey that item out. An item
    // with no action at all is drawn live and does nothing.
    let unavailable = gio::SimpleAction::new("unavailable", None);
    unavailable.set_enabled(false);
    let none = gio::SimpleAction::new("none", None);
    none.set_enabled(false);

    let group = gio::SimpleActionGroup::new();
    for action in [choose.upcast_ref::<gio::Action>(), new.upcast_ref(), delete.upcast_ref(), unavailable.upcast_ref(), none.upcast_ref()] {
        group.add_action(action);
    }
    button.insert_action_group("profile", Some(&group));

    let note = gtk::Label::new(None);
    note.set_wrap(true);
    note.set_justify(gtk::Justification::Center);
    note.add_css_class("caption");
    note.add_css_class("dim-label");
    note.set_visible(false);

    let switcher = Switcher {
        config,
        config_path,
        button: button.clone(),
        label,
        menu,
        choose: choose.clone(),
        delete: delete.clone(),
        note: note.clone(),
    };

    {
        let switcher = switcher.clone();
        choose.connect_activate(move |_, target| {
            if let Some(name) = target.and_then(|t| t.str()) {
                switcher.choose(name);
            }
        });
    }

    // "New Profile…" and "Delete Profile…" are items beside the list rather
    // than entries in it. An action pretending to be a value has to be
    // un-selected again the moment it is chosen, and the reverting is visible:
    // the button briefly reads "New Profile…" as though that were the profile
    // you are about to launch.
    {
        let switcher = switcher.clone();
        new.connect_activate(move |_, _| {
            if let Some(window) = switcher.button.root().and_downcast::<gtk::Window>() {
                create(&window, &switcher);
            }
        });
    }
    {
        let switcher = switcher.clone();
        delete.connect_activate(move |_, _| {
            if let Some(window) = switcher.button.root().and_downcast::<gtk::Window>() {
                delete_current(&window, &switcher);
            }
        });
    }

    // Called by GTK before every popup, which is what makes the availability
    // notes current. Giving the button a function also keeps it sensitive.
    {
        let switcher = switcher.clone();
        button.set_create_popup_func(move |_| switcher.refresh());
    }

    switcher.refresh();

    let refresh: Rc<dyn Fn()> = Rc::new(move || switcher.refresh());
    Chooser { button, note, refresh }
}


/// What the confirmation says. Split from the dialog so the wording, which is
/// the only thing standing between somebody and an unrecoverable deletion, is
/// tested.
///
/// **It names the keyring**, because the sign-in is the part people do not think
/// of as "in the profile", and it says the deletion cannot be undone.
fn delete_body(name: &str, data_bytes: u64) -> String {
    let data = if data_bytes == 0 {
        "Roblox's data for it".to_string()
    } else {
        format!("Roblox's data for it ({})", profile::human_bytes(data_bytes))
    };
    format!(
        "This deletes profile {name:?}: its saved sign-in, including the copy in your desktop \
         keyring, its settings, FastFlags and plugin grants, and {data}. It cannot be undone."
    )
}

/// Confirm, then delete the profile the row shows and move to another.
///
/// **The confirmation is a real one**: a destructive-appearance response that is
/// not the default, so Enter cancels. Deleting the shown profile is allowed,
/// unlike the fork's command line, which refused the current profile: here the
/// shown profile *is* the current one, and refusing would make the only profile
/// undeletable. Afterwards the row moves to whichever profile remains, or back
/// to `default`, which is created when it is next launched.
fn delete_current(parent: &gtk::Window, switcher: &Switcher) {
    let name = switcher.current();
    if !offered().contains(&name) {
        return;
    }
    let bytes = profile::dir(&name).map(|d| profile::engine_data_bytes(&d)).unwrap_or(0);
    let dialog = adw::AlertDialog::builder()
        .heading(format!("Delete profile {name:?}?"))
        .body(delete_body(&name, bytes))
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("delete", "Delete");
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");

    let switcher = switcher.clone();
    let parent = parent.clone();
    let present_parent = parent.clone();
    dialog.connect_response(None, move |_, response| {
        if response != "delete" {
            return;
        }
        match profile::remove(&name) {
            Ok(removed) => {
                let next = offered().into_iter().next().unwrap_or_else(|| "default".to_string());
                switcher.choose(&next);
                switcher.refresh();
                if let Some(why) = removed.keyring_unreached {
                    crate::window::alert(
                        &parent,
                        "Profile deleted, but not its keyring entry",
                        &format!(
                            "The desktop keyring was not available ({why}), so a sign-in kept there for \
                             this profile could not be erased. Remove it in your keyring application \
                             (Seahorse, KeePassXC) under \"Cordial\"."
                        ),
                    );
                }
            }
            Err(e) => crate::window::alert(&parent, "Cordial could not delete that profile", &e.to_string()),
        }
    });
    dialog.present(Some(&present_parent));
}

/// Make a profile, or say why not.
///
/// Creation goes through [`profile::acquire`] — the same door a launch uses —
/// so a name that cannot be made into a usable directory fails here with the
/// message it would have failed with there, and the directory comes out `0700`
/// because that is where the mode is applied. The claim is dropped at once; this
/// window is not an instance.
fn create(parent: &gtk::Window, switcher: &Switcher) {
    let entry = adw::EntryRow::builder().title("Name").build();
    let group = adw::PreferencesGroup::new();
    group.add(&entry);

    let hint = gtk::Label::new(None);
    hint.set_xalign(0.0);
    hint.set_wrap(true);
    hint.add_css_class("caption");
    hint.add_css_class("dim-label");

    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.append(&group);
    body.append(&hint);

    let dialog = adw::AlertDialog::builder()
        .heading("New profile")
        .body(
            "A profile is one account's Roblox storage: its own session, settings, flag \
             overrides and plugin grants. Creating one does not sign you in — Cordial \
             selects a directory and never sees a password (ADR-012).",
        )
        .extra_child(&body)
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("create", "Create");
    dialog.set_response_appearance("create", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("create"));
    dialog.set_close_response("cancel");
    // Nothing typed is nothing to create. The button follows what has been
    // typed rather than accepting it and reporting afterwards, because a
    // refusal after the dialog has closed is a refusal with nowhere to correct
    // it.
    dialog.set_response_enabled("create", false);

    {
        let dialog = dialog.clone();
        let hint = hint.clone();
        entry.connect_changed(move |entry| {
            let name = entry.text().to_string();
            match check_name(&name, &offered()) {
                NameCheck::Empty => {
                    entry.remove_css_class("error");
                    hint.set_text("");
                    dialog.set_response_enabled("create", false);
                }
                NameCheck::Invalid(message) => {
                    entry.add_css_class("error");
                    hint.set_text(&message);
                    dialog.set_response_enabled("create", false);
                }
                NameCheck::Existing => {
                    entry.remove_css_class("error");
                    hint.set_text("That profile already exists; this will switch to it.");
                    dialog.set_response_enabled("create", true);
                }
                NameCheck::New => {
                    entry.remove_css_class("error");
                    hint.set_text("");
                    dialog.set_response_enabled("create", true);
                }
            }
        });
    }

    // Kept for `present`, which needs the parent after the closure below has
    // taken its own copy.
    let present_parent = parent.clone();
    let switcher = switcher.clone();
    let parent = parent.clone();
    // The entry itself is captured, rather than found again from the dialog's
    // widget tree when Create is pressed. The tree walk was written first, on
    // the argument that reaching the live widget cannot disagree with a stale
    // copy — but a captured `AdwEntryRow` *is* the live widget, and the walk
    // silently returned nothing: pressing Create reported `"" is not a usable
    // profile name` for a perfectly good one. Found by pressing the button,
    // which is the only way it could have been.
    let typed = entry.clone();
    dialog.connect_response(None, move |_, response| {
        if response != "create" {
            return;
        }
        let name = typed.text().to_string();
        match profile::acquire(&name) {
            Ok(claim) => {
                drop(claim);
                switcher.choose(&name);
                switcher.refresh();
            }
            // Both remaining cases already have a sentence written for them on
            // `profile::Error`, and this is not the place to write a second one.
            Err(e) => crate::window::alert(&parent, "Cordial could not open that profile", &e.to_string()),
        }
    });

    dialog.present(Some(&present_parent));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> (PathBuf, std::sync::MutexGuard<'static, ()>) {
        // Shared with `launch.rs`: `CORDIAL_PROFILE_ROOT` is process-wide, and
        // a mutex private to this file only stops this file's own tests
        // interleaving, not another file's in the same binary. See
        // `crate::PROFILE_ROOT_ENV`'s own doc for the flake this fixed.
        let guard = crate::PROFILE_ROOT_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let p = std::env::temp_dir().join(format!("cordial-switcher-test-{tag}"));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        std::env::set_var("CORDIAL_PROFILE_ROOT", &p);
        (p, guard)
    }

    #[test]
    fn a_running_profile_is_shown_as_unavailable_rather_than_offered() {
        // The whole reason the probe is `acquire` and not a check of this
        // module's own: what the list says has to be what the launch will do,
        // and only the lock knows that.
        let (_root, _g) = scratch("running");
        let held = profile::acquire("main").expect("a fresh profile is free");
        assert_eq!(availability("main"), Availability::Running);
        drop(held);
        // Allowed a moment to come free. Other tests in this binary spawn
        // processes, and a child forked while the lock is still open shares it
        // until its own exec closes the descriptor, so an immediate re-probe
        // could read Running for a few milliseconds. Failed that way once in a
        // full workspace run, 2026-09-25; it passes alone.
        let freed = (0..50).any(|_| {
            let free = availability("main") == Availability::Free;
            if !free {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            free
        });
        assert!(freed, "a released profile must read as free again");
    }

    #[test]
    fn probing_does_not_leave_the_profile_held() {
        // The hazard in answering the question by taking the lock. If the probe
        // kept it, opening the dropdown would make every profile in it
        // unlaunchable — the chooser would be the instance holding them.
        let (_root, _g) = scratch("release");
        assert_eq!(availability("main"), Availability::Free);
        profile::acquire("main").expect("the probe must have let go again");
    }

    #[test]
    fn an_impossible_name_is_refused_in_profiles_own_words() {
        // Not "matches an error", but "is the same sentence". A second wording
        // for the same refusal is how a user ends up believing there are two
        // different rules.
        let (_root, _g) = scratch("names");
        let expected = profile::dir("has/slash").unwrap_err();
        assert_eq!(check_name("has/slash", &[]), NameCheck::Invalid(expected));
        assert_eq!(check_name("../escape", &[]), NameCheck::Invalid(profile::dir("../escape").unwrap_err()));
    }

    #[test]
    fn every_profile_the_list_offers_is_one_the_create_dialog_would_accept() {
        // The trap this rules out: a name that can exist on disk but cannot be
        // typed. Both ends go through `profile::is_valid_name` — `list` filters
        // on it and `dir` refuses on it — so they agree by construction, and
        // this is here so that loosening one without the other fails loudly.
        //
        // It is not hypothetical. `default.testruns` is in this developer's own
        // profile root, a dot is not in the allowed set, and the consequence is
        // that the directory is invisible to `list` as well as untypeable —
        // consistent, and worth knowing, which is why the case is written down
        // rather than assumed.
        let (root, _g) = scratch("agree");
        for name in ["default", "alt_account-2", "default.testruns", "fdsafdsagfdsgfdgfdgfd"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
        }
        let listed = offered();
        assert!(!listed.iter().any(|n| n == "default.testruns"), "a dot is not a usable name: {listed:?}");
        for name in &listed {
            assert!(
                !matches!(check_name(name, &[]), NameCheck::Invalid(_)),
                "{name} is offered by the list but refused by the create dialog"
            );
        }
    }

    #[test]
    fn the_confirmation_names_the_profile_the_keyring_and_that_it_cannot_be_undone() {
        let body = delete_body("alt", 0);
        assert!(body.contains("\"alt\""), "{body}");
        assert!(body.contains("keyring"), "{body}");
        assert!(body.contains("cannot be undone"), "{body}");
        assert!(delete_body("alt", 2_500_000_000).contains("2.5 GB"), "{}", delete_body("alt", 2_500_000_000));
    }

    #[test]
    fn a_pin_is_said_even_where_the_row_would_otherwise_say_nothing() {
        assert_eq!(with_pin(String::new(), None), "");
        assert_eq!(with_pin(String::new(), Some("2.738.0.1397")), "Pinned to Roblox 2.738.0.1397");
        assert_eq!(
            with_pin("Opened in another window".into(), Some("2.738.0.1397")),
            "Opened in another window. Pinned to Roblox 2.738.0.1397"
        );
    }

    #[test]
    fn a_profile_that_is_free_has_nothing_to_say_about_itself() {
        // The regression this is here to stop, because it is the sort of line
        // that gets pasted back by somebody who thinks a blank subtitle is an
        // oversight. It read "One account's Roblox storage, held by one window
        // at a time" — ADR-012's definition of the word, true of every entry in
        // the list, and the largest single reason a launcher looked like a
        // settings page.
        assert_eq!(subtitle("default", Some(&Availability::Free)), "");
    }

    #[test]
    fn the_cases_that_change_what_the_button_does_all_still_say_so() {
        // Dropping the definition must not drop the three that are worth
        // reading. Each of these changes what pressing Roblox will do.
        assert!(subtitle("alt", None).contains("will be created"), "an uncreated profile has to say so");
        // No longer asserts "refused": the row does not promise that any more,
        // because the button is live for a busy profile on purpose and the
        // dialog behind it offers to close the other client. What the row still
        // has to say is that the profile is in use somewhere.
        let running = subtitle("alt", Some(&Availability::Running));
        assert!(running.contains("another window"), "{running}");
        // Verbatim, for the same reason `check_name` quotes `profile::dir`: the
        // sentence a user meets is the one the operating system produced.
        assert_eq!(subtitle("alt", Some(&Availability::Unusable("permission denied".into()))), "permission denied");
    }

    #[test]
    fn nothing_typed_is_not_an_error_and_is_not_creatable_either() {
        let (_root, _g) = scratch("empty");
        assert_eq!(check_name("", &[]), NameCheck::Empty);
    }

    #[test]
    fn a_name_that_already_exists_is_a_switch_rather_than_a_create() {
        let (_root, _g) = scratch("existing");
        let existing = vec!["default".to_string()];
        assert_eq!(check_name("default", &existing), NameCheck::Existing);
        assert_eq!(check_name("alt_account-2", &existing), NameCheck::New);
    }

    #[test]
    fn the_list_offers_no_profile_that_does_not_exist() {
        // The regression this exists for. A profile is a signed-in session, so
        // an entry for one that is not on disk is a launcher claiming an account
        // the user does not have — and the first version of this module did
        // exactly that, synthesising the chosen profile so that a fresh install
        // would not show an empty list. An empty list is the correct answer to
        // an empty profile root.
        let (root, _g) = scratch("nothing");
        assert!(offered().is_empty(), "an empty profile root must offer nothing at all");

        // And exactly what is there once something is, in `profile::list`'s own
        // order, with nothing added on either side.
        std::fs::create_dir_all(root.join("main")).unwrap();
        std::fs::create_dir_all(root.join("alt")).unwrap();
        assert_eq!(offered(), vec!["alt".to_string(), "main".to_string()]);
    }

    #[test]
    fn an_underscore_in_a_name_is_not_a_mnemonic() {
        // `profile::is_valid_name` allows underscores, and a menu item reads a
        // single one as "underline the next letter", so `alt_account` would be
        // drawn as "altaccount". Doubled, GTK draws one.
        assert_eq!(menu_label("alt_account-2"), "alt__account-2");
        assert_eq!(menu_label("default"), "default");
    }

    #[test]
    fn a_long_name_keeps_both_ends_and_a_short_one_is_untouched() {
        assert_eq!(shorten("default", 40), "default");
        let long = "fdsafdsagfdsgfdgfdgfdfdsafdsagfdsgfdgfdgfdfdsafdsagfdsgfdgfdgfd9";
        let short = shorten(long, 20);
        assert_eq!(short.chars().count(), 20, "{short}");
        assert!(short.starts_with("fdsafdsag") && short.ends_with("gfd9"), "{short}");
        assert!(short.contains('…'), "{short}");
        // Counted in characters: a name is ASCII today, an operating-system
        // message need not be, and slicing it by bytes would panic.
        assert_eq!(shorten("ééééééééé", 5).chars().count(), 5);
    }

    #[test]
    fn a_menu_item_says_what_choosing_it_will_run_into() {
        assert_eq!(item_text("main", &Availability::Free), ("main".to_string(), true));
        // Still choosable: the dialog behind the button offers to close the
        // client holding it, and a greyed item would hide the one profile that
        // needs that.
        assert_eq!(
            item_text("main", &Availability::Running),
            ("main (opened in another window)".to_string(), true)
        );
        let (text, choosable) = item_text("main", &Availability::Unusable("permission denied".into()));
        assert!(!choosable, "nothing can fix an unusable profile from here");
        assert!(text.contains("permission denied"), "{text}");
    }
}
