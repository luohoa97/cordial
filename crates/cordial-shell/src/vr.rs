//! What "Play in VR" needs from the machine, found without changing any of it.
//!
//! The VR mode runs Roblox's Quest build in the ordinary client, under the
//! in-process translator, inside an OpenXR session
//! ([ADR-053](../../../docs/adr/ADR-053-vr-is-a-mode-of-the-android-runtime.md)).
//! Three things have to be true before the launcher offers it: an x86-64 host,
//! a Quest build the user imported, and an OpenXR runtime to reach. This module
//! answers each, and says what is missing in words a person can act on.
//!
//! **Read-only, all of it.** The OpenXR runtime is found by reading the
//! loader's own files and the usual install locations, and the choice is
//! handed to one client in `XR_RUNTIME_JSON`. Nothing here writes
//! `active_runtime.json`, starts a VR server, or installs anything: those
//! belong to the runtime's own tools, and an application that quietly switched
//! the machine's OpenXR runtime would break the next OpenXR program the user
//! starts for a reason they could not trace back to Cordial.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Whether this build of Cordial can run the Quest build at all.
///
/// The translator has an x86-64 host backend only, and `cordial-run` refuses
/// `--guest-arm64` elsewhere. The launcher hides the VR entry rather than
/// greying it out on other hosts: there is nothing the user can do about it.
pub const HOST_SUPPORTED: bool = cfg!(target_arch = "x86_64");

/// The WiVRn Flatpak's application id.
pub const WIVRN_FLATPAK: &str = "io.github.wivrn.wivrn";

/// The SteamVR install's manifest, relative to the Steam library.
const STEAMVR_MANIFEST: &str = "steamapps/common/SteamVR/steamxr_linux64.json";

/// One OpenXR runtime found on the machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Runtime {
    /// What a setting stores: stable across updates, unlike the manifest path
    /// of a Flatpak, which names a commit.
    pub id: String,
    /// The manifest's own `runtime.name`, or the file name if it has none.
    pub name: String,
    pub manifest: PathBuf,
}

/// Where to look, so tests can point every lookup at a scratch directory.
#[derive(Debug, Clone, Default)]
pub struct Places {
    pub home: Option<PathBuf>,
    pub config_home: Option<PathBuf>,
    pub config_dirs: Vec<PathBuf>,
    pub data_home: Option<PathBuf>,
    pub data_dirs: Vec<PathBuf>,
    /// `flatpak info --show-location io.github.wivrn.wivrn`, already asked,
    /// or the install found on disk where `flatpak` cannot be run.
    pub wivrn_flatpak: Option<PathBuf>,
    /// Inside Cordial's own Flatpak. Three things differ there:
    /// `XDG_CONFIG_HOME` is the app's own, so the user's active runtime is
    /// read from `~/.config` beside it; `/proc` shows only the sandbox, so a
    /// running WiVRn server is found by its socket instead; and the loader in
    /// the sandbox would read the app's config, so even the system's choice
    /// is passed in `XR_RUNTIME_JSON`.
    pub sandboxed: bool,
    /// `XDG_RUNTIME_DIR`, where WiVRn's server puts its socket.
    pub runtime_dir: Option<PathBuf>,
}

impl Places {
    /// The places the OpenXR loader and the usual packages use on this
    /// machine, with the XDG defaults the specification gives when a variable
    /// is unset.
    pub fn of_this_machine() -> Self {
        let mut places = Self::from_files();
        if let Some(asked) = wivrn_flatpak_location() {
            places.wivrn_flatpak = Some(asked);
        }
        places
    }

    /// The same places, found without starting a process: WiVRn's Flatpak is
    /// looked for in the two default installations on disk rather than by
    /// asking `flatpak info`. What the launcher's main screen uses, at
    /// start-up and on every return to the front, where a subprocess on the
    /// GTK thread is the cost #75 was asked to avoid. A WiVRn in a custom
    /// Flatpak installation is missed here and found by Settings → VR.
    pub fn from_files() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let dirs = |var: &str, default: &str| -> Vec<PathBuf> {
            let v = std::env::var(var)
                .ok()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| default.to_string());
            v.split(':')
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .collect()
        };
        let under_home = |var: &str, rel: &str| {
            std::env::var_os(var)
                .map(PathBuf::from)
                .or_else(|| home.as_ref().map(|h| h.join(rel)))
        };
        Places {
            config_home: under_home("XDG_CONFIG_HOME", ".config"),
            config_dirs: dirs("XDG_CONFIG_DIRS", "/etc/xdg"),
            data_home: under_home("XDG_DATA_HOME", ".local/share"),
            data_dirs: dirs("XDG_DATA_DIRS", "/usr/local/share:/usr/share"),
            wivrn_flatpak: flatpak_install_on_disk(home.as_deref(), Path::new("/var/lib/flatpak"), WIVRN_FLATPAK),
            sandboxed: Path::new("/.flatpak-info").exists(),
            runtime_dir: std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from),
            home,
        }
    }
}

/// WiVRn's Flatpak location, asked once per process, for Settings → VR, a
/// launch, `--doctor` and `--diagnostics`. The launcher's own entry does not
/// ask at all: it runs on every return to the front and uses
/// [`Places::from_files`].
fn wivrn_flatpak_location() -> Option<PathBuf> {
    static ASKED: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    ASKED
        .get_or_init(|| flatpak_location(WIVRN_FLATPAK))
        .clone()
}

/// `flatpak info --show-location <app>`, or `None` if Flatpak or the app is
/// absent. Asked rather than guessed, because a user installation and a
/// system one live in different trees.
pub fn flatpak_location(app: &str) -> Option<PathBuf> {
    let out = Command::new("flatpak")
        .args(["info", "--show-location", app])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let line = String::from_utf8(out.stdout).ok()?;
    let line = line.trim();
    (!line.is_empty()).then(|| PathBuf::from(line))
}

/// Where a Flatpak app is installed, read from disk: the user installation
/// first, as `flatpak` itself prefers it, then the system one. For Cordial's
/// own Flatpak, which has no `flatpak` command, and sees these directories
/// only through the read-only grants in its manifest. `current/active` is
/// the deployed commit, the same tree `flatpak info --show-location` names.
pub fn flatpak_install_on_disk(home: Option<&Path>, system: &Path, app: &str) -> Option<PathBuf> {
    let user = home.map(|h| h.join(".local/share/flatpak"));
    user.into_iter()
        .chain([system.to_path_buf()])
        .map(|root| root.join("app").join(app).join("current/active"))
        .find(|p| p.join("files").is_dir())
}

/// The runtime the OpenXR loader would pick with nothing overriding it: the
/// first `openxr/1/active_runtime.json` in the user's config home, then each
/// config dir, then `/etc` -- the loader's own order.
pub fn system_active(places: &Places) -> Option<Runtime> {
    let mut candidates = Vec::new();
    candidates.extend(places.config_home.iter().cloned());
    if places.sandboxed {
        // The user's own `~/.config`, which the manifest grants read-only as
        // `xdg-config/openxr`; `XDG_CONFIG_HOME` here is the app's.
        candidates.extend(places.home.iter().map(|h| h.join(".config")));
    }
    candidates.extend(places.config_dirs.iter().cloned());
    candidates.push(PathBuf::from("/etc"));
    candidates
        .into_iter()
        .map(|d| d.join("openxr/1/active_runtime.json"))
        .find(|p| p.is_file())
        .map(|manifest| Runtime {
            id: SYSTEM.into(),
            name: manifest_name(&manifest),
            manifest,
        })
}

/// The setting's value for "whatever the system's active runtime is".
pub const SYSTEM: &str = "system";

/// Every runtime installed where runtimes are usually installed, whether or not
/// it is the active one.
pub fn detect(places: &Places) -> Vec<Runtime> {
    let mut found: Vec<Runtime> = Vec::new();
    let mut push = |id: String, manifest: PathBuf| {
        if manifest.is_file() && !found.iter().any(|r| r.manifest == manifest) {
            found.push(Runtime {
                id,
                name: manifest_name(&manifest),
                manifest,
            });
        }
    };
    if let Some(location) = &places.wivrn_flatpak {
        push(
            WIVRN_FLATPAK.into(),
            location.join("files/share/openxr/1/openxr_wivrn.json"),
        );
    }
    if let Some(home) = &places.home {
        for steam in [".local/share/Steam", ".steam/steam"] {
            push("steamvr".into(), home.join(steam).join(STEAMVR_MANIFEST));
        }
    }
    // Packages put their manifest in `share/openxr/1/` -- Monado's and a
    // native WiVRn's both -- even though the loader itself only reads the
    // active one. Every `.json` there that is a runtime manifest is offered,
    // named by its file so the id survives the package being updated.
    let mut shares = Vec::new();
    shares.extend(places.data_home.iter().cloned());
    shares.extend(places.data_dirs.iter().cloned());
    for share in shares {
        let Ok(entries) = std::fs::read_dir(share.join("openxr/1")) else {
            continue;
        };
        let mut manifests: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "json") && is_runtime_manifest(p))
            .collect();
        manifests.sort();
        for m in manifests {
            let id = m
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("runtime")
                .to_string();
            push(id, m);
        }
    }
    found
}

fn is_runtime_manifest(path: &Path) -> bool {
    read_manifest(path).is_some_and(|v| v.get("runtime").is_some())
}

fn read_manifest(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// The manifest's own `runtime.name`, or its file name when it has none.
pub fn manifest_name(path: &Path) -> String {
    read_manifest(path)
        .and_then(|v| {
            v.pointer("/runtime/name")
                .and_then(|n| n.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| {
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
}

/// The library a manifest names, resolved as the loader resolves it: an
/// absolute path as given, a relative one against the manifest's directory.
pub fn manifest_library(manifest: &Path) -> Option<PathBuf> {
    let v = read_manifest(manifest)?;
    let lib = PathBuf::from(v.pointer("/runtime/library_path")?.as_str()?);
    Some(if lib.is_absolute() {
        lib
    } else {
        manifest.parent()?.join(lib)
    })
}

/// What a launch should use, from the setting's value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Chosen {
    /// Hand the client this manifest in `XR_RUNTIME_JSON`.
    Manifest(Runtime),
    /// The system's active runtime: set nothing, and let the loader read it.
    System(Runtime),
    /// Nothing usable, and why.
    Missing(String),
}

/// Resolve the setting: `None` or [`SYSTEM`] for the active runtime, a
/// detected runtime's id, or an absolute path to a manifest the user chose.
pub fn resolve(setting: Option<&str>, places: &Places) -> Chosen {
    let checked = |r: Runtime, system: bool| -> Chosen {
        match manifest_library(&r.manifest) {
            Some(lib) if lib.is_file() => {
                if system {
                    Chosen::System(r)
                } else {
                    Chosen::Manifest(r)
                }
            }
            Some(lib) => Chosen::Missing(format!(
                "The OpenXR runtime {} names {}, which does not exist.",
                r.name,
                lib.display()
            )),
            None => Chosen::Missing(format!(
                "{} is not an OpenXR runtime manifest.",
                r.manifest.display()
            )),
        }
    };
    match setting {
        None | Some(SYSTEM) => match system_active(places) {
            Some(r) => checked(r, true),
            None => Chosen::Missing(
                "No OpenXR runtime is active on this computer. Install one such as WiVRn, or choose a \
                 runtime in Settings."
                    .into(),
            ),
        },
        Some(path) if path.starts_with('/') => {
            let manifest = PathBuf::from(path);
            if !manifest.is_file() {
                return Chosen::Missing(format!("The chosen OpenXR runtime {path} no longer exists."));
            }
            checked(Runtime { id: path.into(), name: manifest_name(&manifest), manifest }, false)
        }
        Some(id) => match detect(places).into_iter().find(|r| r.id == id) {
            Some(r) => checked(r, false),
            None => Chosen::Missing(format!(
                "The chosen OpenXR runtime ({id}) is no longer installed. Choose another in Settings."
            )),
        },
    }
}

/// Whether a WiVRn server is running, by its process name in `/proc`: the
/// same way `profile::a_client_is_running` asks about `cordial-run`, and for
/// the same reason -- a record kept here could be stale, `/proc` cannot.
pub fn wivrn_server_running() -> bool {
    process_running_in(Path::new("/proc"), "wivrn-server")
}

/// WiVRn's server socket, under `XDG_RUNTIME_DIR`. Inside Cordial's Flatpak
/// `/proc` shows only the sandbox, so the socket the manifest grants
/// (`xdg-run/wivrn`) is the evidence instead. A socket left behind by a
/// server that crashed reads as running; the launch then fails to connect
/// and says so, which is the honest outcome of a stale answer.
pub const WIVRN_SOCKET: &str = "wivrn/comp_ipc";

pub fn wivrn_socket_present(runtime_dir: Option<&Path>) -> bool {
    runtime_dir.is_some_and(|d| d.join(WIVRN_SOCKET).exists())
}

fn process_running_in(proc_root: &Path, name: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(proc_root) else {
        return false;
    };
    entries.flatten().any(|e| {
        e.file_name()
            .to_str()
            .is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()))
            && std::fs::read_to_string(e.path().join("comm")).is_ok_and(|c| c.trim_end() == name)
    })
}

/// Whether a resolved runtime is WiVRn, whose server has to be running before
/// a session can start.
pub fn is_wivrn(r: &Runtime) -> bool {
    r.name.eq_ignore_ascii_case("wivrn")
        || r.id == WIVRN_FLATPAK
        || r.manifest.ends_with("openxr_wivrn.json")
}

/// How to start WiVRn's server without it taking over the machine's active
/// runtime: Cordial passes the runtime per launch, so nothing else needs to
/// change. Shown to the user rather than run, because the launcher starts no
/// long-lived helpers and owning a VR server's lifetime would be a design of
/// its own.
pub fn wivrn_start_command(places: &Places) -> String {
    if places.wivrn_flatpak.is_some() {
        format!("flatpak run --command=wivrn-server {WIVRN_FLATPAK} --no-manage-active-runtime")
    } else {
        "wivrn-server --no-manage-active-runtime".into()
    }
}

/// Whether any OpenXR runtime is on the machine at all: the system's active
/// one, one installed where runtimes usually are, or a manifest the user
/// chose that still exists. Files only, and the cheapest question first.
pub fn any_runtime(setting: Option<&str>, places: &Places) -> bool {
    system_active(places).is_some()
        || setting.is_some_and(|s| s.starts_with('/') && Path::new(s).is_file())
        || !detect(places).is_empty()
}

/// What the launcher's main screen shows for VR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LauncherEntry {
    /// Nothing: no headset software on this machine, so no button to grey out.
    Hidden,
    /// Only the "Set Up VR…" link, which opens Settings → VR.
    SetUp,
    /// The "Play in VR" button.
    Play,
}

/// What the launcher's choice is made from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Found {
    /// [`any_runtime`].
    pub runtime: bool,
    /// A Quest build has been imported.
    pub quest_build: bool,
    /// The setting resolves to a usable runtime: the system's active one when
    /// nothing is chosen, otherwise the one chosen. False when runtimes are
    /// installed but none is active and none has been picked.
    pub runtime_chosen: bool,
}

/// The launcher's VR entry, decided in one place. Every condition for
/// showing it belongs here, so a check for whether VR is compiled in at all
/// is one more line in this function rather than a second guess elsewhere.
pub fn launcher_entry(found: Found) -> LauncherEntry {
    launcher_entry_on(HOST_SUPPORTED, found)
}

fn launcher_entry_on(host_supported: bool, found: Found) -> LauncherEntry {
    if !host_supported || !found.runtime {
        LauncherEntry::Hidden
    } else if found.quest_build && found.runtime_chosen {
        LauncherEntry::Play
    } else {
        LauncherEntry::SetUp
    }
}

/// What the VR entry says when no Quest build has been imported, which is the
/// first thing missing on every machine that has never been set up for VR.
pub const NO_QUEST_BUILD: &str = "Import the Quest build of Roblox from your headset in Settings → VR.";

/// Everything the VR entry needs, gathered once.
#[derive(Debug, Clone)]
pub struct Readiness {
    pub quest_build: Option<cordial_update::store::Entry>,
    pub runtime: Chosen,
    /// `Some(running)` when the chosen runtime is WiVRn.
    pub wivrn_server: Option<bool>,
    /// From [`Places::sandboxed`].
    pub sandboxed: bool,
}

impl Readiness {
    pub fn gather(setting: Option<&str>) -> Self {
        Self::gather_at(setting, &Places::of_this_machine(), cordial_update::quest::current())
    }

    /// [`gather`](Self::gather) at `places`, with the store already read.
    pub fn gather_at(
        setting: Option<&str>,
        places: &Places,
        quest_build: Option<cordial_update::store::Entry>,
    ) -> Self {
        let (sandboxed, runtime_dir) = (places.sandboxed, places.runtime_dir.clone());
        Self::gather_in(setting, places, quest_build, move || {
            if sandboxed {
                wivrn_socket_present(runtime_dir.as_deref())
            } else {
                wivrn_server_running()
            }
        })
    }

    pub fn gather_in(
        setting: Option<&str>,
        places: &Places,
        quest_build: Option<cordial_update::store::Entry>,
        wivrn_running: impl Fn() -> bool,
    ) -> Self {
        let runtime = resolve(setting, places);
        let wivrn_server = match &runtime {
            Chosen::Manifest(r) | Chosen::System(r) if is_wivrn(r) => Some(wivrn_running()),
            _ => None,
        };
        Readiness {
            quest_build,
            runtime,
            wivrn_server,
            sandboxed: places.sandboxed,
        }
    }

    /// What stops a launch, one sentence each, in the order to fix them.
    pub fn missing(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.quest_build.is_none() {
            out.push(NO_QUEST_BUILD.into());
        }
        if let Chosen::Missing(why) = &self.runtime {
            out.push(why.clone());
        }
        if self.wivrn_server == Some(false) {
            out.push(
                "The WiVRn server is not running. Start WiVRn and connect your headset.".into(),
            );
        }
        out
    }

    pub fn ready(&self) -> bool {
        self.missing().is_empty()
    }

    /// The manifest to put in `XR_RUNTIME_JSON`, if the choice is not the
    /// system's own -- or always, inside the Flatpak, where the loader would
    /// look for the system's choice in the app's own config and not find it.
    pub fn manifest_for_launch(&self) -> Option<PathBuf> {
        match &self.runtime {
            Chosen::Manifest(r) => Some(r.manifest.clone()),
            Chosen::System(r) if self.sandboxed => Some(r.manifest.clone()),
            _ => None,
        }
    }

    /// One line for `--diagnostics`.
    pub fn summary(&self) -> String {
        let build = match &self.quest_build {
            Some(e) => format!("Quest build {}", e.version),
            None => "no Quest build".into(),
        };
        let runtime = match &self.runtime {
            Chosen::Manifest(r) => format!("OpenXR {} (chosen)", r.name),
            Chosen::System(r) => format!("OpenXR {} (system)", r.name),
            Chosen::Missing(_) => "no usable OpenXR runtime".into(),
        };
        let server = match self.wivrn_server {
            Some(true) => ", WiVRn server running",
            Some(false) => ", WiVRn server not running",
            None => "",
        };
        format!("{build}, {runtime}{server}")
    }
}

/// What the crash page adds when the run was a VR one.
///
/// Conditional, like the NVIDIA hints beside it, and for an honest reason: an
/// old build that Roblox has stopped accepting is the commonest way a VR
/// session that worked last week stops working, and **no engine signal for it
/// has been observed on the Quest path** -- the phone build's `app upgrade
/// status` line comes from a GameActivity hook the Quest build never calls
/// (ADR-053). So the page says what to try rather than claiming a cause.
pub fn crash_hint(command_line: &str) -> Option<String> {
    command_line.contains("--guest-arm64").then(|| {
        "If Roblox updated recently, this Quest build may no longer be accepted. Update Roblox on \
         your Quest from the Meta Horizon Store, then copy it again in Settings → VR → Get It from \
         Your Quest."
            .to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "cordial-shell-vr-test-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn manifest(path: &Path, name: &str, library: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            path,
            format!(r#"{{"file_format_version":"1.0.0","runtime":{{"name":"{name}","library_path":"{library}"}}}}"#),
        )
        .unwrap();
    }

    fn library(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"").unwrap();
    }

    fn places(root: &Path) -> Places {
        Places {
            home: Some(root.join("home")),
            config_home: Some(root.join("home/.config")),
            config_dirs: vec![root.join("etc/xdg")],
            data_home: Some(root.join("home/.local/share")),
            data_dirs: vec![root.join("usr/share")],
            wivrn_flatpak: None,
            sandboxed: false,
            runtime_dir: None,
        }
    }

    #[test]
    fn the_users_active_runtime_wins_over_the_systems() {
        let root = scratch("active");
        let p = places(&root);
        library(&root.join("lib/a.so"));
        manifest(
            &root.join("etc/xdg/openxr/1/active_runtime.json"),
            "SystemWide",
            &root.join("lib/a.so").display().to_string(),
        );
        assert_eq!(system_active(&p).unwrap().name, "SystemWide");
        manifest(
            &root.join("home/.config/openxr/1/active_runtime.json"),
            "Mine",
            &root.join("lib/a.so").display().to_string(),
        );
        assert_eq!(system_active(&p).unwrap().name, "Mine");
        match resolve(None, &p) {
            Chosen::System(r) => assert_eq!(r.name, "Mine"),
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn wivrn_steamvr_and_packaged_runtimes_are_found_and_resolve_by_id() {
        let root = scratch("detect");
        let mut p = places(&root);
        let flatpak = root.join("flatpak/wivrn/x86_64/stable/abc");
        manifest(
            &flatpak.join("files/share/openxr/1/openxr_wivrn.json"),
            "WiVRn",
            "../../../lib/wivrn/libopenxr_wivrn.so",
        );
        library(&flatpak.join("files/lib/wivrn/libopenxr_wivrn.so"));
        p.wivrn_flatpak = Some(flatpak.clone());
        manifest(
            &root.join("home/.local/share/Steam").join(STEAMVR_MANIFEST),
            "SteamVR",
            "/nonexistent/vrclient.so",
        );
        manifest(
            &root.join("usr/share/openxr/1/openxr_monado.json"),
            "Monado",
            "/x/libopenxr_monado.so",
        );
        // Not a runtime manifest: an API layer's. Must not be offered.
        std::fs::write(
            root.join("usr/share/openxr/1/layer.json"),
            r#"{"api_layer":{}}"#,
        )
        .unwrap();

        let ids: Vec<_> = detect(&p).into_iter().map(|r| r.id).collect();
        assert_eq!(ids, [WIVRN_FLATPAK, "steamvr", "openxr_monado"]);

        // A relative library path resolves against the manifest, as the loader does it.
        match resolve(Some(WIVRN_FLATPAK), &p) {
            Chosen::Manifest(r) => assert!(is_wivrn(&r)),
            other => panic!("{other:?}"),
        }
        // A runtime whose library is gone is reported, not offered as ready.
        assert!(
            matches!(resolve(Some("steamvr"), &p), Chosen::Missing(m) if m.contains("does not exist"))
        );
        assert!(matches!(resolve(Some("gone"), &p), Chosen::Missing(_)));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn nothing_is_written_while_detecting() {
        let root = scratch("readonly");
        let p = places(&root);
        let before = walk(&root);
        let _ = detect(&p);
        let _ = resolve(None, &p);
        assert_eq!(walk(&root), before);
        assert!(matches!(resolve(None, &p), Chosen::Missing(_)));
        let _ = std::fs::remove_dir_all(&root);
    }

    fn walk(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(dir) {
            for e in entries.flatten() {
                out.push(e.path());
                out.extend(walk(&e.path()));
            }
        }
        out.sort();
        out
    }

    #[test]
    fn readiness_names_each_missing_piece_and_the_wivrn_server() {
        let root = scratch("ready");
        let mut p = places(&root);
        let flatpak = root.join("fp");
        manifest(
            &flatpak.join("files/share/openxr/1/openxr_wivrn.json"),
            "WiVRn",
            "../../../lib/wivrn/libopenxr_wivrn.so",
        );
        library(&flatpak.join("files/lib/wivrn/libopenxr_wivrn.so"));
        p.wivrn_flatpak = Some(flatpak);

        let none = Readiness::gather_in(None, &p, None, || true);
        assert_eq!(none.missing().len(), 2, "{:?}", none.missing());
        assert!(!none.ready());

        let entry = cordial_update::store::Entry {
            version: "2.740.0.927".into(),
            dir: root.join("b"),
            loaded_by: None,
            bytes: 0,
            complete: true,
            content_hash: None,
        };
        let stopped = Readiness::gather_in(Some(WIVRN_FLATPAK), &p, Some(entry.clone()), || false);
        assert_eq!(stopped.wivrn_server, Some(false));
        assert!(stopped.missing()[0].contains("WiVRn server"));
        let up = Readiness::gather_in(Some(WIVRN_FLATPAK), &p, Some(entry), || true);
        assert!(up.ready());
        assert!(up
            .manifest_for_launch()
            .unwrap()
            .ends_with("openxr_wivrn.json"));
        assert!(
            up.summary().contains("WiVRn server running"),
            "{}",
            up.summary()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn inside_the_flatpak_the_users_config_and_the_socket_are_read_and_the_choice_is_passed() {
        let root = scratch("sandbox");
        let mut p = places(&root);
        // The app's own XDG_CONFIG_HOME, which has no OpenXR config, and the
        // user's ~/.config beside it, which does.
        p.config_home = Some(root.join("home/.var/app/io.github.luohoa97.Cordial/config"));
        library(&root.join("lib/w.so"));
        manifest(&root.join("home/.config/openxr/1/active_runtime.json"), "WiVRn", &root.join("lib/w.so").to_string_lossy());
        let users = root.join("home/.config/openxr/1/active_runtime.json");
        assert_ne!(
            system_active(&p).map(|r| r.manifest),
            Some(users.clone()),
            "outside a sandbox ~/.config is XDG_CONFIG_HOME's business, not a second place to look"
        );

        p.sandboxed = true;
        let entry = cordial_update::store::Entry {
            version: "2.740.0.927".into(),
            dir: root.join("b"),
            loaded_by: None,
            bytes: 0,
            complete: true,
            content_hash: None,
        };
        let r = Readiness::gather_in(None, &p, Some(entry), || true);
        assert!(matches!(r.runtime, Chosen::System(_)), "{:?}", r.runtime);
        assert_eq!(
            r.manifest_for_launch().as_deref(),
            Some(users.as_path()),
            "the sandbox's loader would not find the system's choice by itself"
        );

        assert!(!wivrn_socket_present(Some(&root.join("run"))));
        std::fs::create_dir_all(root.join("run/wivrn")).unwrap();
        std::fs::write(root.join("run/wivrn/comp_ipc"), b"").unwrap();
        assert!(wivrn_socket_present(Some(&root.join("run"))));
        assert!(!wivrn_socket_present(None));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_flatpak_install_is_found_on_disk_user_first() {
        let root = scratch("fpdisk");
        let home = root.join("home");
        let system = root.join("var/lib/flatpak");
        assert_eq!(flatpak_install_on_disk(Some(&home), &system, WIVRN_FLATPAK), None);
        let sys = system.join("app").join(WIVRN_FLATPAK).join("current/active");
        std::fs::create_dir_all(sys.join("files")).unwrap();
        assert_eq!(flatpak_install_on_disk(Some(&home), &system, WIVRN_FLATPAK), Some(sys));
        let user = home.join(".local/share/flatpak/app").join(WIVRN_FLATPAK).join("current/active");
        std::fs::create_dir_all(user.join("files")).unwrap();
        assert_eq!(flatpak_install_on_disk(Some(&home), &system, WIVRN_FLATPAK), Some(user));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_process_is_found_by_its_comm_and_nothing_else() {
        let root = scratch("proc");
        for (pid, comm) in [
            ("12", "bash\n"),
            ("40", "wivrn-server\n"),
            ("self", "wivrn-server\n"),
        ] {
            std::fs::create_dir_all(root.join(pid)).unwrap();
            std::fs::write(root.join(pid).join("comm"), comm).unwrap();
        }
        assert!(process_running_in(&root, "wivrn-server"));
        std::fs::remove_dir_all(root.join("40")).unwrap();
        assert!(
            !process_running_in(&root, "wivrn-server"),
            "a non-numeric entry is not a process"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_launcher_entry_for_every_combination() {
        use LauncherEntry::*;
        for host in [true, false] {
            for runtime in [true, false] {
                for quest_build in [true, false] {
                    for runtime_chosen in [true, false] {
                        let found = Found { runtime, quest_build, runtime_chosen };
                        let want = match (host, runtime, quest_build, runtime_chosen) {
                            (false, ..) => Hidden,
                            (true, false, ..) => Hidden,
                            (true, true, true, true) => Play,
                            (true, true, _, _) => SetUp,
                        };
                        assert_eq!(launcher_entry_on(host, found), want, "host={host} {found:?}");
                    }
                }
            }
        }
        if !HOST_SUPPORTED {
            let all = Found { runtime: true, quest_build: true, runtime_chosen: true };
            assert_eq!(launcher_entry(all), Hidden);
        }
    }

    #[test]
    fn any_runtime_is_answered_from_files() {
        let root = scratch("any");
        let mut p = places(&root);
        assert!(!any_runtime(None, &p));
        // A manifest the user picked counts while it exists.
        let picked = root.join("picked.json");
        assert!(!any_runtime(Some(picked.to_str().unwrap()), &p));
        manifest(&picked, "Picked", "/x.so");
        assert!(any_runtime(Some(picked.to_str().unwrap()), &p));
        // An id names a runtime only if detection finds it.
        assert!(!any_runtime(Some("steamvr"), &p));
        manifest(&root.join("usr/share/openxr/1/openxr_monado.json"), "Monado", "/x.so");
        assert!(any_runtime(None, &p));
        std::fs::remove_dir_all(root.join("usr")).unwrap();
        manifest(&root.join("etc/xdg/openxr/1/active_runtime.json"), "Active", "/x.so");
        assert!(any_runtime(None, &p));
        std::fs::remove_dir_all(root.join("etc")).unwrap();
        // WiVRn's Flatpak, as found on disk.
        let fp = root.join("fp");
        manifest(&fp.join("files/share/openxr/1/openxr_wivrn.json"), "WiVRn", "../../../lib/w.so");
        p.wivrn_flatpak = Some(fp);
        assert!(any_runtime(None, &p));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn inside_the_flatpak_the_users_active_runtime_shows_the_entry() {
        let root = scratch("anysandbox");
        let mut p = places(&root);
        p.config_home = Some(root.join("home/.var/app/io.github.luohoa97.Cordial/config"));
        manifest(&root.join("home/.config/openxr/1/active_runtime.json"), "WiVRn", "/x.so");
        assert!(!any_runtime(None, &p), "outside a sandbox ~/.config is not read");
        p.sandboxed = true;
        assert!(any_runtime(None, &p));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn only_a_vr_run_gets_the_update_hint() {
        assert!(crash_hint("/x/cordial-run --lib-dir /b --apk /b/base.apk --host-libc --guest-arm64 --app-bridge --run 0").is_some());
        assert!(crash_hint(
            "/x/cordial-run --lib-dir /b --apk /b/base.apk --host-libc --game-activity --run 0"
        )
        .is_none());
    }
}
