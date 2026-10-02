//! Settings → VR: the Quest build and the OpenXR runtime.
//!
//! Two groups, for the two things "Play in VR" needs that a user supplies
//! (ADR-053). The Quest build comes from the user's own headset -- Cordial
//! ships no Roblox code and downloads no Quest build from anywhere -- either
//! as an APK file they already have or pulled over `adb`. The runtime is
//! chosen here and handed to one launch at a time; the system's active runtime
//! is read and never written.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use libadwaita as adw;
use libadwaita::glib;
use libadwaita::gtk;
use libadwaita::prelude::*;

use crate::settings::{choose_file, persist};
use crate::shell_config::ShellConfig;
use cordial_shell::vr;
use cordial_update::quest;

pub fn build_vr_page(
    dialog: &adw::PreferencesDialog,
    parent: &gtk::Window,
    config: Rc<RefCell<ShellConfig>>,
    config_path: Rc<PathBuf>,
) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("VR")
        .name("vr")
        .icon_name("view-dual-symbolic")
        .description(
            "Play in VR runs Roblox's Meta Quest build on this computer and shows it in your headset \
             through an OpenXR runtime such as WiVRn. The launcher shows Set Up VR once a runtime is \
             installed, and Play in VR once the Quest build is imported and a runtime is chosen \
             below. Text entry, haptics and leaving a game are not finished yet; use an alt account.",
        )
        .build();
    page.add(&build_quest_group(dialog, parent));
    page.add(&build_runtime_group(parent, config, config_path));
    page
}

fn quest_status() -> String {
    match quest::current() {
        Some(e) => format!("Roblox {}, imported", e.version),
        None => "Not imported yet".into(),
    }
}

fn build_quest_group(
    dialog: &adw::PreferencesDialog,
    parent: &gtk::Window,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Quest build")
        .description(
            "The Roblox app from your own Quest. Cordial never downloads it; it checks that Roblox \
             signed it, then keeps it with the other Roblox builds.",
        )
        .build();

    let status = adw::ActionRow::builder()
        .title("Quest build")
        .subtitle(quest_status())
        .build();
    group.add(&status);

    // First and recommended: the steps that copy it off the headset, which is
    // where the user's own copy is.
    // The same steps update it. Roblox stops accepting an old build after it
    // updates, and the fix is always the same: update on the headset, copy
    // again. Said here because no signal for it reaches Cordial (ADR-053).
    let pull = adw::ActionRow::builder()
        .title("Get It from Your Quest")
        .subtitle(if quest::current().is_some() {
            "Also how to update it. When Roblox updates, an old version can stop being playable: \
             update Roblox on your Quest from the Meta Horizon Store, then copy it again here."
        } else {
            "Recommended. Copies Roblox off your headset over a USB cable, step by step."
        })
        .activatable(true)
        .build();
    let start = gtk::Button::builder()
        .label("Start")
        .valign(gtk::Align::Center)
        .css_classes(["suggested-action"])
        .build();
    pull.add_suffix(&start);
    pull.set_activatable_widget(Some(&start));
    group.add(&pull);
    {
        let dialog = dialog.clone();
        let status = status.clone();
        start.connect_clicked(move |_| {
            let status = status.clone();
            crate::quest_wizard::open(
                &dialog,
                Rc::new(move |version: String| {
                    status.set_subtitle(&format!("Roblox {version}, imported"))
                }),
            );
        });
    }

    // The photograph seam (`window::open_on_start`): `vr-pull=<step>` in
    // `CORDIAL_SHELL_PRESENT` opens the steps at that one.
    if let Some(at) = std::env::var("CORDIAL_SHELL_PRESENT").ok().and_then(|v| {
        v.split(',')
            .find_map(|p| p.trim().strip_prefix("vr-pull=").map(str::to_string))
    }) {
        let dialog = dialog.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(400), move || {
            crate::quest_wizard::open_at(&dialog, Rc::new(|_| {}), &at);
        });
    }

    let import = adw::ActionRow::builder()
        .title("I Have the APK File")
        .subtitle("A Quest build of Roblox you copied off your own headset yourself.")
        .build();
    let choose = gtk::Button::builder()
        .label("Choose…")
        .valign(gtk::Align::Center)
        .build();
    import.add_suffix(&choose);
    group.add(&import);
    {
        let parent = parent.clone();
        let status = status.clone();
        choose.connect_clicked(move |b| {
            let status = status.clone();
            let b = b.clone();
            choose_file(&parent, "Choose the Quest APK", false, move |path| {
                b.set_sensitive(false);
                status.set_subtitle("Checking Roblox's signature and filing the build…");
                let status = status.clone();
                let b = b.clone();
                // A 140 MB copy and a hash of a 110 MB engine: about a second,
                // and not on the main thread.
                crate::updater::on_worker_reporting(
                    move |_: &dyn Fn(())| {
                        quest::import(
                            &path,
                            &quest::root(),
                            &cordial_update::apk_signature::pinned_quest(),
                        )
                        .map_err(|e| e.to_string())
                    },
                    |_| {},
                    move |result| {
                        b.set_sensitive(true);
                        status.set_subtitle(&match result {
                            Ok(version) => format!("Roblox {version}, imported"),
                            Err(why) => format!("{}\n{why}", quest_status()),
                        });
                    },
                );
            });
        });
    }
    group
}

/// The runtime row's subtitle when no OpenXR runtime is installed at all.
const NO_RUNTIME: &str = "No OpenXR runtime found. Play in VR needs one to reach your headset: \
     install WiVRn (or SteamVR or Monado), then reopen Settings. Until then the launcher shows no \
     VR button.";

/// The runtime choices, in combo order: the system's, each detected runtime,
/// and a manifest the user picked if it is not one of those.
fn choices(setting: Option<&str>, places: &vr::Places) -> Vec<(String, Option<String>)> {
    let system = match vr::system_active(places) {
        Some(r) => format!("System default ({})", r.name),
        None => "System default (none active)".into(),
    };
    let mut out = vec![(system, None)];
    for r in vr::detect(places) {
        out.push((format!("{} — {}", r.name, r.manifest.display()), Some(r.id)));
    }
    if let Some(path) = setting.filter(|s| s.starts_with('/')) {
        if !out.iter().any(|(_, id)| id.as_deref() == Some(path)) {
            out.push((
                format!("{} — {path}", vr::manifest_name(std::path::Path::new(path))),
                Some(path.to_string()),
            ));
        }
    }
    out
}

fn build_runtime_group(
    parent: &gtk::Window,
    config: Rc<RefCell<ShellConfig>>,
    config_path: Rc<PathBuf>,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("OpenXR runtime")
        .description(
            "Given to the game for each launch only. Cordial never changes which runtime your \
             computer uses for everything else.",
        )
        .build();
    let places = vr::Places::of_this_machine();
    let setting = config.borrow().vr_openxr_runtime.clone();
    let options = choices(setting.as_deref(), &places);
    let labels: Vec<&str> = options.iter().map(|(l, _)| l.as_str()).collect();
    let selected = options
        .iter()
        .position(|(_, id)| id.as_deref() == setting.as_deref())
        .unwrap_or(0);
    let combo = adw::ComboRow::builder()
        .title("Runtime")
        .model(&gtk::StringList::new(&labels))
        .selected(selected as u32)
        .build();
    group.add(&combo);

    let server = adw::ActionRow::builder().title("WiVRn server").build();
    group.add(&server);
    // What the chosen runtime needs, said on the rows themselves: a runtime
    // whose library is gone on the combo, WiVRn's server on its own row.
    let show_status = {
        let server = server.clone();
        let combo = combo.clone();
        let config = config.clone();
        Rc::new(move || {
            let places = vr::Places::of_this_machine();
            let readiness = vr::Readiness::gather_in(
                config.borrow().vr_openxr_runtime.as_deref(),
                &places,
                None,
                vr::wivrn_server_running,
            );
            let setting = config.borrow().vr_openxr_runtime.clone();
            // With nothing installed the resolver's sentence ("choose a
            // runtime in Settings") points back at this row, so the
            // requirement is stated instead: this is the only place a person
            // without a runtime can find out why the launcher shows no VR.
            if !vr::any_runtime(setting.as_deref(), &places) {
                combo.set_subtitle(NO_RUNTIME);
            } else {
                combo.set_subtitle(match &readiness.runtime {
                    vr::Chosen::Missing(why) => why,
                    _ => "",
                });
            }
            server.set_visible(readiness.wivrn_server.is_some());
            server.set_subtitle(&match readiness.wivrn_server {
                Some(true) => "Running".to_string(),
                _ => format!(
                    "Not running. Start WiVRn, or run: {}",
                    vr::wivrn_start_command(&places)
                ),
            });
        })
    };
    show_status();

    // Shared with the picker below, which rebuilds the list: the handler must
    // read the ids of the model it is looking at, not the one it was built with.
    let ids: Rc<RefCell<Vec<Option<String>>>> = Rc::new(RefCell::new(
        options.iter().map(|(_, id)| id.clone()).collect(),
    ));
    {
        let config = config.clone();
        let config_path = config_path.clone();
        let ids = ids.clone();
        let show_status = show_status.clone();
        combo.connect_selected_notify(move |row| {
            let id = ids.borrow().get(row.selected() as usize).cloned().flatten();
            config.borrow_mut().vr_openxr_runtime = id;
            persist(&config, &config_path);
            show_status();
        });
    }

    let other = adw::ActionRow::builder()
        .title("Another runtime")
        .subtitle("Choose an OpenXR runtime manifest (.json) that is not listed above.")
        .build();
    let choose = gtk::Button::builder()
        .label("Choose…")
        .valign(gtk::Align::Center)
        .build();
    other.add_suffix(&choose);
    group.add(&other);
    {
        let parent = parent.clone();
        choose.connect_clicked(move |_| {
            let config = config.clone();
            let config_path = config_path.clone();
            let combo = combo.clone();
            let ids = ids.clone();
            choose_file(
                &parent,
                "Choose an OpenXR runtime manifest",
                false,
                move |path| {
                    let path = path.display().to_string();
                    // Rebuilt rather than patched: the list is short, and one
                    // construction site cannot disagree with itself. The ids go
                    // first, so the selection handler that `set_model` and
                    // `set_selected` fire reads the new list.
                    let options = choices(Some(&path), &vr::Places::of_this_machine());
                    *ids.borrow_mut() = options.iter().map(|(_, id)| id.clone()).collect();
                    let labels: Vec<&str> = options.iter().map(|(l, _)| l.as_str()).collect();
                    combo.set_model(Some(&gtk::StringList::new(&labels)));
                    let at = options
                        .iter()
                        .position(|(_, id)| id.as_deref() == Some(path.as_str()))
                        .unwrap_or(0);
                    combo.set_selected(at as u32);
                    // Written whatever the notifications did: `set_selected` to the
                    // index already selected fires nothing.
                    config.borrow_mut().vr_openxr_runtime = Some(path.clone());
                    persist(&config, &config_path);
                },
            );
        });
    }
    group
}
