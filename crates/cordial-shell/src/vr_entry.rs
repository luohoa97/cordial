//! "Play in VR", under the Roblox button.
//!
//! A second, quieter control rather than a split button or a menu: the main
//! button keeps its one job and its one look, and the VR entry has something
//! the main button never needs, which is a sentence underneath saying what is
//! missing. A split button's arrow would hide that sentence behind a click, and
//! an insensitive menu item cannot explain itself at all -- the rule
//! `settings::detail` states for rows, applied here: an unavailable control
//! keeps its reason on screen.
//!
//! Shown only to somebody who has headset software. With no OpenXR runtime on
//! the machine there is nothing at all here; with one but VR not set up, only
//! the "Set Up VR…" link; once a Quest build is imported and a runtime is
//! chosen, the button. [`cordial_shell::vr::launcher_entry`] decides, and
//! Settings → VR is there in every case (ADR-053).

use std::cell::RefCell;
use std::rc::Rc;

use libadwaita::glib;
use libadwaita::gtk;
use libadwaita::prelude::*;

use crate::shell_config::ShellConfig;
use cordial_shell::vr;

pub struct VrEntry {
    pub widget: gtk::Widget,
    /// Re-reads the runtimes, the store and WiVRn's server, and shows, hides
    /// or relabels the entry to match. Called when Settings closes and when
    /// the window comes back to the front, which are the two moments any of
    /// them is likely to have changed.
    pub refresh: Rc<dyn Fn()>,
}

pub fn build(config: Rc<RefCell<ShellConfig>>, on_play: impl Fn() + 'static) -> Option<VrEntry> {
    if !cordial_shell::vr::HOST_SUPPORTED {
        return None;
    }
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    content.set_halign(gtk::Align::Center);
    content.append(&gtk::Image::from_icon_name("view-dual-symbolic"));
    content.append(&gtk::Label::new(Some("Play in VR")));
    let button = gtk::Button::builder()
        .child(&content)
        .css_classes(["pill"])
        .build();
    button.connect_clicked(move |_| on_play());

    let caption = gtk::Label::builder()
        .wrap(true)
        .justify(gtk::Justification::Center)
        .max_width_chars(44)
        .css_classes(["dim-label", "caption"])
        .build();
    let setup = gtk::Button::builder()
        .label("Set Up VR…")
        .halign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    setup.connect_clicked(|b| {
        let _ = b.activate_action("win.settings", Some(&"vr".to_variant()));
    });

    let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
    column.append(&button);
    column.append(&caption);
    column.append(&setup);

    let widget: gtk::Widget = column.clone().upcast();
    let refresh: Rc<dyn Fn()> = Rc::new(move || {
        // Files only, and `/proc` for WiVRn's server: no `flatpak info`, which
        // `Places::of_this_machine` would start, because this runs at start-up
        // and on every return to the front, on the GTK thread. The runtime
        // question comes first and is a handful of `stat`s; the setting is
        // resolved only when there is a Quest build for it to matter to.
        let places = vr::Places::from_files();
        let setting = config.borrow().vr_openxr_runtime.clone();
        let runtime = vr::any_runtime(setting.as_deref(), &places);
        let quest_build = if runtime { cordial_update::quest::current() } else { None };
        let readiness = quest_build
            .is_some()
            .then(|| vr::Readiness::gather_at(setting.as_deref(), &places, quest_build));
        let found = vr::Found {
            runtime,
            quest_build: readiness.is_some(),
            runtime_chosen: readiness
                .as_ref()
                .is_some_and(|r| !matches!(r.runtime, vr::Chosen::Missing(_))),
        };
        let entry = vr::launcher_entry(found);
        column.set_visible(entry != vr::LauncherEntry::Hidden);
        button.set_visible(entry == vr::LauncherEntry::Play);
        caption.set_visible(entry == vr::LauncherEntry::Play);
        setup.set_visible(entry == vr::LauncherEntry::SetUp);
        if let (vr::LauncherEntry::Play, Some(readiness)) = (entry, &readiness) {
            // Set up, but something can still be missing for this launch --
            // WiVRn's server not running is the usual one -- and that is said
            // beneath the button rather than hidden.
            let missing = readiness.missing();
            button.set_sensitive(missing.is_empty());
            caption.set_label(&match missing.first() {
                Some(first) => first.clone(),
                None => readiness.summary(),
            });
        }
    });
    refresh();
    Some(VrEntry {
        widget,
        refresh,
    })
}

/// Keep `entry` current while the window is in use: on every return to the
/// front, which is when a headset may have been plugged in or WiVRn started.
pub fn follow_window(window: &impl IsA<gtk::Window>, refresh: Rc<dyn Fn()>) {
    window.connect_is_active_notify(move |w| {
        if w.is_active() {
            let refresh = refresh.clone();
            // Off the notify itself: gathering asks `/proc` and the
            // filesystem, and a property notification is not the place for it.
            glib::idle_add_local_once(move || refresh());
        }
    });
}
