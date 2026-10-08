//! The one screen for "something went wrong": the diagnostics block, a way to
//! copy or save it, and the way to the issue tracker.
//!
//! **This used to be two screens showing the same text.** Settings had a
//! Report page with the block, a Copy button and an issue link; the About
//! dialog had its own Troubleshooting page with the block, Copy and Save. Both
//! read `diagnostics::report`, so they never disagreed about the machine, but a
//! reporter who found one did not know the other existed, and a page in
//! Settings that is not a setting was the wrong home for either.
//!
//! It is a dialog of its own rather than the About dialog's Troubleshooting
//! page because `AdwAboutDialog` has no way to open on a chosen page, and the
//! launcher's X11 notice has to land on exactly this screen. The About dialog
//! links here instead of carrying a second copy.

use cordial_shell::doctor::{Check, Level};
use libadwaita as adw;
use libadwaita::glib;
use libadwaita::gtk;
use libadwaita::prelude::*;
use crate::shell_config::ShellConfig;
use std::cell::RefCell;
use std::rc::Rc;

/// Where a report goes. One constant, because the row here and the About
/// dialog's link both name it and two copies drift.
pub const ISSUES_URL: &str = "https://github.com/luohoa97/cordial/issues/new/choose";

/// Where "Open an issue" goes, and a note when that is not obvious.
///
/// **An unofficial build reports to the repository it was built from when that
/// is known**, since the project cannot see what a fork changed and a report
/// filed upstream about somebody else's patch costs both sides an evening. When
/// it is not known -- a tarball, a distro package, a checkout with a local-path
/// remote -- the link stays upstream's and the note says the build is not
/// official; the diagnostics block carries the same fact in its Build line, so
/// the report is marked whichever way it is sent. A hint for triage only: see
/// `version::Origin`.
pub fn issues_target(origin: &cordial_shell::version::Origin) -> (String, Option<&'static str>) {
    use cordial_shell::version::UPSTREAM_REPO;
    if origin.is_official() {
        return (ISSUES_URL.to_string(), None);
    }
    match origin.github_slug() {
        Some(slug) if !slug.eq_ignore_ascii_case(UPSTREAM_REPO) => (
            format!("https://github.com/{slug}/issues/new/choose"),
            Some("Unofficial build: this goes to the repository it was built from."),
        ),
        _ => (
            ISSUES_URL.to_string(),
            Some("Unofficial build: mention that when you report it. The diagnostics say so."),
        ),
    }
}

/// The file name the Save button offers. The same one the About dialog's own
/// Troubleshooting page offered, so anyone who knew it does not have to learn
/// a second.
const SAVE_NAME: &str = "cordial-diagnostics.txt";

/// The file name the Save logs button offers. A `.zip` because that is what a
/// GitHub issue accepts as an attachment without being renamed first.
const LOGS_NAME: &str = "cordial-logs.zip";

/// One line each, shared by the row and the prompt before the issue form, so
/// the two cannot describe the archive differently.
const LOGS_SUBTITLE: &str = "A .zip to attach to the issue: the diagnostics, this session's launcher output, \
the last client run and the engine's newest logs. Cookies, tickets, link queries, user ids, \
names and typed text are removed first.";

/// Ask where to put the log archive, build it, write it.
///
/// **The build runs on a worker.** It reads up to two engine logs of a couple
/// of megabytes each and asks `coredumpctl` a question, none of which belongs on
/// the thread that draws this dialog. `done` is told whether a file was written,
/// and is also called when the picker is dismissed, because the issue form
/// opens after this whichever way it went.
fn save_logs(
    parent: Option<gtk::Window>,
    toasts: adw::ToastOverlay,
    profile: String,
    diagnostics: String,
    done: impl FnOnce(bool) + 'static,
) {
    gtk::FileDialog::builder().title("Save logs").initial_name(LOGS_NAME).build().save(
        parent.as_ref(),
        gtk::gio::Cancellable::NONE,
        move |result| {
            let Ok(file) = result else { return done(false) };
            glib::MainContext::default().spawn_local(async move {
                let built =
                    gtk::gio::spawn_blocking(move || cordial_shell::log_export::export(&profile, diagnostics)).await;
                let message = match built {
                    Ok(Ok(bytes)) => match file.replace_contents(
                        &bytes,
                        None,
                        false,
                        gtk::gio::FileCreateFlags::NONE,
                        gtk::gio::Cancellable::NONE,
                    ) {
                        Ok(_) => {
                            done(true);
                            "Saved. Drag the .zip into the issue."
                        }
                        Err(e) => {
                            eprintln!("[cordial] could not save the logs: {e}");
                            done(false);
                            "The logs could not be saved."
                        }
                    },
                    Ok(Err(e)) => {
                        eprintln!("[cordial] could not build the log archive: {e}");
                        done(false);
                        "The logs could not be collected."
                    }
                    Err(_) => {
                        done(false);
                        "The logs could not be collected."
                    }
                };
                toasts.add_toast(adw::Toast::new(message));
            });
        },
    );
}

/// Builds the report screen. Nothing is shown until it is presented.
pub fn build(config: Rc<RefCell<ShellConfig>>) -> adw::Dialog {
    let block = crate::diagnostics::report();
    // What Copy and Save hand over. Starts as the block alone and gains the
    // doctor's checks when they finish, so a press before that still copies
    // something true rather than waiting on a driver.
    let text = Rc::new(RefCell::new(block.clone()));

    let page = adw::PreferencesPage::new();

    let group = adw::PreferencesGroup::builder()
        .title("Diagnostics")
        // One line. The block says what it contains by containing it, and the
        // reasoning about what is deliberately absent lives in `diagnostics.rs`
        // next to the code that decides it.
        .description("Paste this into a GitHub issue, and attach the logs.")
        .build();

    // Monospace and selectable: the columns only line up in a fixed-width font,
    // and somebody who wants one line rather than the block should be able to
    // take it without the button's all-or-nothing.
    let view = gtk::TextView::builder()
        .editable(false)
        .monospace(true)
        .cursor_visible(false)
        // **Wrapped, because `uname -a` is longer than any dialog.** Without it
        // the System line ran off the edge with no scrollbar, and somebody
        // checking what they were about to paste into a public issue could not
        // read the one line most likely to make them think twice. `WordChar`
        // rather than `Word`: a kernel version has no spaces to break at.
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(12)
        .bottom_margin(12)
        .left_margin(12)
        .right_margin(12)
        .build();
    view.buffer().set_text(&block);
    let frame = gtk::Frame::new(None);
    frame.set_child(Some(&view));
    frame.set_margin_top(6);

    let copy = gtk::Button::with_label("Copy");
    copy.set_valign(gtk::Align::Center);
    let copied = text.clone();
    copy.connect_clicked(move |b| {
        if let Some(display) = gtk::gdk::Display::default() {
            display.clipboard().set_text(&copied.borrow());
            // The label is the confirmation. A button that looks identical
            // after a press is one people press three times.
            b.set_label("Copied");
            let b = b.clone();
            glib::timeout_add_seconds_local_once(2, move || b.set_label("Copy"));
        }
    });
    let copy_row = adw::ActionRow::builder().title("Copy diagnostics").build();
    copy_row.add_suffix(&copy);
    group.add(&copy_row);

    // Kept from the About dialog's Troubleshooting page, which had it and the
    // Settings page did not: a report attached to an issue as a file survives
    // a paste box that mangles columns.
    let save = gtk::Button::with_label("Save…");
    save.set_valign(gtk::Align::Center);
    let saved = text.clone();
    save.connect_clicked(move |b| {
        let parent = b.root().and_downcast::<gtk::Window>();
        let text = saved.borrow().clone();
        gtk::FileDialog::builder()
            .title("Save diagnostics")
            .initial_name(SAVE_NAME)
            .build()
            .save(parent.as_ref(), gtk::gio::Cancellable::NONE, move |result| {
                // A dismissed picker is an error result too; only a real
                // failure to write is worth saying anything about.
                let Ok(file) = result else { return };
                if let Err(e) = file.replace_contents(
                    text.as_bytes(),
                    None,
                    false,
                    gtk::gio::FileCreateFlags::NONE,
                    gtk::gio::Cancellable::NONE,
                ) {
                    eprintln!("[cordial] could not save the diagnostics: {e}");
                }
            });
    });
    let save_row = adw::ActionRow::builder().title("Save to a file").build();
    save_row.add_suffix(&save);
    group.add(&save_row);

    // The toasts live on the dialog's own overlay, created below with the rest
    // of the layout, and this needs them before then: both buttons that save
    // the archive report back through it.
    let toasts = adw::ToastOverlay::new();

    // **The one thing a bug report needed that this screen did not offer.**
    // Issue threads kept asking for terminal output, the engine's log and a
    // coredump by hand, from people who were not running anything in a
    // terminal. See `log_export` for what goes in and what is taken out.
    let logs = gtk::Button::with_label("Save logs…");
    logs.set_valign(gtk::Align::Center);
    {
        let (toasts, config, text) = (toasts.clone(), config.clone(), text.clone());
        logs.connect_clicked(move |b| {
            let parent = b.root().and_downcast::<gtk::Window>();
            save_logs(parent, toasts.clone(), config.borrow().profile.clone(), text.borrow().clone(), |_| {});
        });
    }
    let logs_row = adw::ActionRow::builder().title("Save logs").subtitle(LOGS_SUBTITLE).build();
    logs_row.add_suffix(&logs);
    group.add(&logs_row);
    group.add(&frame);
    page.add(&group);

    // The doctor's checks. Filled in when they finish: the Vulkan question
    // runs a child process and the D-Bus ones can wait on a session bus, so
    // they are asked on a worker and the screen opens at once.
    let checks_group = adw::PreferencesGroup::builder()
        .title("This machine")
        .description("Checking…")
        .build();
    page.add(&checks_group);
    {
        let checks_group = checks_group.clone();
        let text = text.clone();
        glib::MainContext::default().spawn_local(async move {
            // Offline: opening a report screen is not a reason to make a
            // network request. `cordial --doctor` asks the mirror; this does not.
            let result = gtk::gio::spawn_blocking(|| crate::doctor_run::checks(true)).await;
            match result {
                Ok(checks) => {
                    checks_group.set_description(Some(&cordial_shell::doctor::verdict(&checks)));
                    for check in &checks {
                        checks_group.add(&check_row(check));
                    }
                    *text.borrow_mut() = crate::diagnostics::with_checks(&block, &checks);
                }
                Err(_) => checks_group.set_description(Some("The checks could not be run.")),
            }
        });
    }

    let where_group = adw::PreferencesGroup::builder().title("Bugs and feature requests").build();
    // A link row rather than prose with a URL in it: this is the last place
    // somebody is before they give up, and it should take one press to get from
    // here to the form.
    let (issues_url, issues_note) = issues_target(&cordial_shell::version::origin());
    let subtitle = match issues_note {
        Some(note) => format!("{}\n{note}", issues_url.trim_start_matches("https://")),
        None => issues_url.trim_start_matches("https://").to_string(),
    };
    let issues = adw::ActionRow::builder()
        .title("Open an issue")
        .subtitle(glib::markup_escape_text(&format!("{subtitle}\nSave the logs first and attach them.")))
        .activatable(true)
        .build();
    // `go-next-symbolic`, checked on disk rather than guessed: the first
    // attempt used `external-link-symbolic`, which is in no icon theme here and
    // rendered as the missing-image glyph.
    issues.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    {
        let (toasts, config, text) = (toasts.clone(), config.clone(), text.clone());
        issues.connect_activated(move |row| {
            let parent = row.root().and_downcast::<gtk::Window>();
            // **Offered before the form opens, not after.** The form is in a
            // browser, so there is no moment afterwards at which this window
            // can still say "and attach the logs", and a required field that
            // says to drag a .zip in is no help to someone who has not got one.
            let ask = adw::AlertDialog::builder()
                .heading("Attach the logs?")
                .body(
                    "A report is much easier to act on with the logs. Cordial can save them as a .zip \
                     now, with cookies, tickets, user ids, names and typed text removed, so you can \
                     drag it into the form.",
                )
                .build();
            ask.add_responses(&[("cancel", "Cancel"), ("skip", "Open Without Logs"), ("save", "Save Logs and Open")]);
            ask.set_response_appearance("save", adw::ResponseAppearance::Suggested);
            ask.set_default_response(Some("save"));
            ask.set_close_response("cancel");
            let (toasts, config, text, url) = (toasts.clone(), config.clone(), text.clone(), issues_url.clone());
            let window = parent.clone();
            ask.choose(parent.as_ref(), gtk::gio::Cancellable::NONE, move |response| match response.as_str() {
                "skip" => open_issue_form(window.as_ref(), &url),
                "save" => {
                    let profile = config.borrow().profile.clone();
                    let diagnostics = text.borrow().clone();
                    let opener = window.clone();
                    // Opens the form whether or not a file was written: a
                    // picker dismissed or a full disk is no reason to strand
                    // someone who pressed the button to report a problem.
                    save_logs(window, toasts, profile, diagnostics, move |_| {
                        open_issue_form(opener.as_ref(), &url)
                    });
                }
                _ => {}
            });
        });
    }
    where_group.add(&issues);
    page.add(&where_group);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&page));

    toasts.set_child(Some(&toolbar));

    adw::Dialog::builder()
        .title("Report a Problem")
        .content_width(520)
        .content_height(680)
        .child(&toasts)
        .build()
}

/// **`GtkUriLauncher`, not `cordial_plugins::urlopen`, and the difference is
/// focus.** Both reach `org.freedesktop.portal.OpenURI`, so both work inside
/// the Flatpak sandbox. But `urlopen` is the plugin path: a plugin has no
/// window, so it passes an empty parent handle and no activation token, and
/// GNOME's focus-stealing prevention answers by declining to raise the
/// browser. This row has a window to offer, and `UriLauncher::launch` hands
/// the portal what it needs to raise the browser properly.
fn open_issue_form(parent: Option<&gtk::Window>, url: &str) {
    gtk::UriLauncher::new(url).launch(parent, gtk::gio::Cancellable::NONE, |result| {
        if let Err(e) = result {
            eprintln!("[cordial] could not open the issue tracker: {e}");
        }
    });
}

/// One check as a row: the finding as the title, what to do about it as the
/// subtitle, and an icon that carries the level for somebody who does not read
/// the words first. The text goes through `private` and is escaped, because a
/// row's title and subtitle are Pango markup and a GPU or driver name may
/// contain `&` or `<`.
fn check_row(check: &Check) -> adw::ActionRow {
    let (icon, class) = match check.level {
        Level::Ok => ("object-select-symbolic", "success"),
        Level::Info => ("dialog-information-symbolic", "dim-label"),
        Level::Warn => ("dialog-warning-symbolic", "warning"),
        Level::Fail => ("dialog-error-symbolic", "error"),
    };
    let row = adw::ActionRow::builder()
        .title(glib::markup_escape_text(&cordial_shell::doctor::private(&check.what)))
        .build();
    if !check.fix.trim().is_empty() {
        row.set_subtitle(&glib::markup_escape_text(&cordial_shell::doctor::private(check.fix.trim())));
    }
    let image = gtk::Image::from_icon_name(icon);
    image.add_css_class(class);
    row.add_prefix(&image);
    row
}

/// Puts the report screen up over `parent`.
///
/// `config` is read when a button is pressed, not now: the profile whose engine
/// logs go in the archive is the one chosen at that moment.
pub fn present(parent: &impl IsA<gtk::Widget>, config: &Rc<RefCell<ShellConfig>>) {
    build(config.clone()).present(Some(parent));
}

#[cfg(test)]
mod tests {
    use super::*;
    use cordial_shell::version::Origin;

    #[test]
    fn an_official_build_reports_upstream_with_no_note() {
        assert_eq!(issues_target(&Origin::Official), (ISSUES_URL.to_string(), None));
    }

    #[test]
    fn an_unofficial_build_reports_to_the_repository_it_came_from() {
        let fork = Origin::Unofficial { remote: Some("https://github.com/somebody/cordial".into()) };
        let (url, note) = issues_target(&fork);
        assert_eq!(url, "https://github.com/somebody/cordial/issues/new/choose");
        assert!(note.is_some());
    }

    /// Unknown, or not GitHub, or a checkout of upstream itself: upstream's
    /// link, marked. Never a guessed address on another host.
    #[test]
    fn where_the_origin_is_unknown_the_link_stays_upstreams_and_is_marked() {
        for origin in [
            Origin::Unofficial { remote: None },
            Origin::Unofficial { remote: Some("https://codeberg.org/a/b".into()) },
            Origin::Unofficial { remote: Some("https://github.com/luohoa97/cordial".into()) },
            Origin::Unofficial { remote: Some("https://github.com/Luohoa97/Cordial".into()) },
        ] {
            let (url, note) = issues_target(&origin);
            assert_eq!(url, ISSUES_URL, "{origin:?}");
            assert!(note.is_some_and(|n| n.contains("Unofficial")), "{origin:?}");
        }
    }
}
