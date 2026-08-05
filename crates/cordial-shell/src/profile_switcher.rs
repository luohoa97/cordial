//! Choosing the profile the next instance runs.
//!
//! An `AdwComboRow` sitting directly above the Launch button, listing
//! [`profile::list`]. ADR-012 makes a profile a directory and an instance a
//! window; this is where one is picked for the other.
//!
//! **Why here and not the header bar.** The first version of this was an
//! `AdwAvatar` in the top right opening a popover, and it was wrong in a way
//! worth writing down rather than quietly replacing. An avatar in that corner is
//! a *browser* convention — Chrome and Firefox put the profile there — and
//! GNOME's HIG has no profile-switcher pattern at all. Fractal is the one
//! libadwaita precedent and Fractal is an account-centric application, where
//! your identity is the ambient context of everything on screen. Cordial's shell
//! is a launcher, and the profile is a launch parameter: *which of these do I
//! start*. Putting it in the far corner opposite the thing it governs separates
//! the choice from the launch it applies to, which is what made it uncomfortable
//! to look at. It is a row above the button now.
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

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use libadwaita as adw;
use libadwaita::gtk;
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

/// What the row says beneath the name, given what is known about the profile.
///
/// **The ordinary case says nothing, and that is the change.** This used to read
/// "One account's Roblox storage, held by one window at a time" whenever the
/// selected profile was free — which is ADR-012's definition of the word rather
/// than anything the person choosing needs. It was true of every entry in the
/// list, so it distinguished nothing; it taught the data model to somebody who
/// only wanted to press play; and a permanent two-line subtitle over a permanent
/// group header is what made a launcher read as a settings page. An empty
/// subtitle collapses the row to one line, which has the useful side effect that
/// a line appearing at all is now the signal that something is worth reading.
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
        // Not "{name} is open in another window". The row's own value is showing
        // that name a couple of centimetres to the right, so repeating it only
        // cost width — and width is what this line has least of, sharing the row
        // with the combo and the create button. Said in full it ellipsised
        // before reaching "refused", which is the half that matters.
        Some(Availability::Running) => "Open in another window; launching it again will be refused".into(),
        Some(Availability::Unusable(message)) => message.clone(),
    }
}

/// The config, the file it is saved to, and the widgets showing the answer.
#[derive(Clone)]
struct Switcher {
    config: Rc<RefCell<ShellConfig>>,
    config_path: Rc<PathBuf>,
    row: adw::ComboRow,
    model: gtk::StringList,
    /// Set while [`Switcher::refresh`] is rewriting the model, because
    /// `set_selected` emits the same notification a user's choice does. Without
    /// it, repopulating the list writes whatever happens to be at the selected
    /// index back into the config — including nothing at all when the list is
    /// empty, which would replace a perfectly good profile name with an empty
    /// string on the first launch of a fresh install.
    updating: Rc<Cell<bool>>,
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

    /// Rebuild the list from disk and reselect the chosen profile.
    ///
    /// Called when the row is built and after a profile is created, rather than
    /// held as state: a profile can appear or disappear from under this window
    /// at any time, and a list assembled once is confidently wrong by the second
    /// launch.
    fn refresh(&self) {
        let names = offered();
        let current = self.current();
        self.updating.set(true);
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        self.model.splice(0, self.model.n_items(), &refs);
        match names.iter().position(|n| *n == current) {
            Some(index) => self.row.set_selected(index as u32),
            // Nothing to point at. `GTK_INVALID_LIST_POSITION` is how a
            // `GtkSelectionModel` says "no selection", and it is the honest
            // answer when the chosen profile has never been created.
            None => self.row.set_selected(gtk::INVALID_LIST_POSITION),
        }
        self.updating.set(false);
        self.describe();
    }

    /// Say what the launch will actually do, underneath the row. The wording,
    /// and why the ordinary case says nothing at all, is on [`subtitle`].
    fn describe(&self) {
        let name = self.current();
        // Probed only once the profile is known to exist, and that order is not
        // incidental: `profile::acquire` creates the directory on its way to the
        // lock, so asking whether a not-yet-created profile is free would create
        // it — a launcher conjuring an account out of drawing its own subtitle.
        let availability = offered().iter().any(|n| *n == name).then(|| availability(&name));
        self.row.set_subtitle(&subtitle(&name, availability.as_ref()));
    }
}

/// The profile chooser, as a group ready to sit above the Launch button.
pub fn build(config: Rc<RefCell<ShellConfig>>, config_path: Rc<PathBuf>) -> adw::PreferencesGroup {
    let model = gtk::StringList::new(&[]);
    let row = adw::ComboRow::builder().title("Profile").model(&model).build();
    // Three rather than two, and only the `Unusable` case will ever want the
    // third: it carries whatever the operating system said about the directory,
    // which is not a sentence written here and cannot be budgeted for. The
    // ordinary row has no subtitle at all and stays one line high.
    row.set_subtitle_lines(3);
    row.set_list_factory(Some(&list_factory()));

    let switcher = Switcher {
        config,
        config_path,
        row: row.clone(),
        model,
        updating: Rc::new(Cell::new(false)),
    };

    {
        let switcher = switcher.clone();
        row.connect_selected_notify(move |row| {
            if switcher.updating.get() {
                return;
            }
            if let Some(name) = switcher.model.string(row.selected()) {
                switcher.choose(&name);
            }
        });
    }

    // A suffix button rather than a final "New profile…" entry in the list.
    // An action pretending to be a value has to be un-selected again the moment
    // it is chosen, and the reverting is visible: the row briefly reads "New
    // profile…" as though that were the profile you are about to launch.
    let new = gtk::Button::from_icon_name("list-add-symbolic");
    new.set_tooltip_text(Some("New profile…"));
    new.set_valign(gtk::Align::Center);
    new.add_css_class("flat");
    {
        let switcher = switcher.clone();
        new.connect_clicked(move |button| {
            if let Some(window) = button.root().and_downcast::<gtk::Window>() {
                create(&window, &switcher);
            }
        });
    }
    row.add_suffix(&new);

    switcher.refresh();

    let group = adw::PreferencesGroup::new();
    group.add(&row);

    group
}

/// How one profile is drawn inside the dropdown.
///
/// The factory exists for one reason: `GtkListItem` carries `selectable` and
/// `activatable`, and that is the only mechanism in this widget that can show a
/// profile as unavailable rather than offering it and refusing afterwards. A
/// plain `GtkStringList` with the default rendering has no way to say "not this
/// one".
///
/// Availability is asked at bind time rather than when the list was built, so
/// what the dropdown shows is what was true when it was opened.
fn list_factory() -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();

    factory.connect_setup(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
        let line = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let name = gtk::Label::new(None);
        name.set_xalign(0.0);
        name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        // Ellipsising alone does nothing: a `GtkLabel` still asks for its whole
        // natural width and the popup grows to give it. Capping the character
        // count is what bounds that request.
        name.set_max_width_chars(NAME_WIDTH);
        let note = gtk::Label::new(None);
        note.set_xalign(0.0);
        note.set_ellipsize(gtk::pango::EllipsizeMode::End);
        note.set_max_width_chars(NAME_WIDTH);
        note.add_css_class("caption");
        note.add_css_class("dim-label");
        line.append(&name);
        line.append(&note);
        item.set_child(Some(&line));
    });

    factory.connect_bind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
        let Some(line) = item.child().and_downcast::<gtk::Box>() else { return };
        let Some(name_label) = line.first_child().and_downcast::<gtk::Label>() else { return };
        let Some(note_label) = line.last_child().and_downcast::<gtk::Label>() else { return };
        let name = item
            .item()
            .and_downcast::<gtk::StringObject>()
            .map(|s| s.string().to_string())
            .unwrap_or_default();

        name_label.set_text(&name);
        match availability(&name) {
            Availability::Free => {
                item.set_selectable(true);
                item.set_activatable(true);
                line.set_sensitive(true);
                note_label.set_visible(false);
            }
            Availability::Running => {
                item.set_selectable(false);
                item.set_activatable(false);
                line.set_sensitive(false);
                note_label.set_text("Open in another window");
                note_label.set_visible(true);
            }
            Availability::Unusable(message) => {
                item.set_selectable(false);
                item.set_activatable(false);
                line.set_sensitive(false);
                note_label.set_text(&message);
                note_label.set_visible(true);
            }
        }
    });

    factory
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

    let dialog = adw::MessageDialog::builder()
        .transient_for(parent)
        .modal(true)
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

    dialog.present();
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
        assert_eq!(availability("main"), Availability::Free);
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
        let running = subtitle("alt", Some(&Availability::Running));
        assert!(running.contains("another window") && running.contains("refused"), "{running}");
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
}
