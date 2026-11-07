//! Bridging Roblox's accessibility tree to AT-SPI, so Orca and other Linux
//! screen readers can read it.
//!
//! ## What this answers, and what it does not
//!
//! Android apps build a semantic UI tree for TalkBack out of
//! `android.view.accessibility.*` objects; `native/accessibility.cpp` hooks
//! that surface the way every other class in `native/android_classes.cpp`
//! does, and mirrors whatever Roblox's engine populates into a small
//! registry `cordial_linker_sys::accessibility` exposes to Rust. This module
//! is the other half: it turns that registry into `org.a11y.atspi.*` objects
//! on the accessibility bus, which is Linux's equivalent of the Android
//! platform machinery that would otherwise hand TalkBack the same data.
//!
//! **This was written without a Roblox APK available in the environment.**
//! Every claim about what Roblox actually calls is labelled INFERRED in
//! `native/accessibility.cpp`'s own header comment, and nothing in *this*
//! file changes that — it faithfully republishes whatever the mirror
//! contains, including "nothing", which is the honest state until someone
//! runs `--dump-classes`/`CORDIAL_JNI_TRACE=1` against a real client with
//! `CORDIAL_ACCESSIBILITY=1` forced on. What *is* independently verified,
//! against a live AT-SPI bus on the machine this was written on (see the
//! accompanying report): the D-Bus method/property signatures below, the
//! `GetState` bit-packing (`au` as `[low32, high32]`, bit N = `AtspiStateType`
//! ordinal N), and the `AtspiRole`/`AtspiStateType` ordinals themselves, all
//! read from `/usr/include/at-spi-2.0/atspi/atspi-constants.h` and
//! cross-checked with `gdbus introspect`/`gdbus call` against a running GTK4
//! application's own AT-SPI objects.
//!
//! ## Why a hand-rolled D-Bus bridge, not `cordial-shell`'s GTK/libadwaita
//!
//! GTK4 already runs an AT-SPI bridge for its own widgets — implementing
//! `GtkAccessible` on a widget is usually far cheaper than speaking
//! `org.a11y.atspi` by hand, and the task this module was written for asked
//! that trade-off be evaluated explicitly rather than assumed. It does not
//! apply here, for a structural reason: `cordial-shell` is the *chooser and
//! settings* window (ADR-002) and has no dependency on `cordial-runtime` in
//! the other direction — the actual game surface Roblox renders into is a
//! raw Wayland client in `crates/cordial-runtime/src/android/wayland.rs`
//! (registry binding, `xdg_shell`, `wl_seat`, by hand — confirmed by reading
//! that crate's own `Cargo.toml`, which has no GTK/libadwaita dependency at
//! all), not a GTK widget. `wayland.rs`/`window.rs`/`input.rs` were also
//! explicitly out of scope for this change (a live deadlock investigation is
//! using them). Even without that constraint, bolting a `GtkAccessible`
//! implementation onto a window that is not a GTK widget would mean
//! either rewriting the render window in GTK — a far larger, unrelated
//! change — or running a second, invisible GTK widget purely to host
//! accessibility, which is the hand-rolled-D-Bus cost with extra steps and a
//! GTK dependency `cordial-runtime` does not otherwise need. Speaking
//! `org.a11y.atspi` directly, over `zbus` (already a workspace dependency —
//! `cordial-plugins` uses it for portals, pinned at the same 5.18.0), costs
//! one more crate edge and is the same mechanism GTK's own bridge uses
//! underneath, just without the GTK widget tree on top of it.
//!
//! ## Shape of the bridge
//!
//! One D-Bus object per accessible node, each a real (path, interface)
//! registration on `zbus`'s `ObjectServer` rather than one object dispatching
//! by hand — `org.a11y.atspi.Accessible` (every node), `Component` (bounds)
//! and `Action` (whatever `AccessibilityAction`s the node carries), plus one
//! `Application` object at `/org/a11y/atspi/accessible/root`, which is also
//! what gets `Embed`-ed into the desktop's own accessible tree so Cordial
//! shows up as an application at all.
//!
//! **The tree is flat.** Real `AccessibilityNodeInfo` has no
//! object-to-object child API (`addChild` takes a `View`/virtual-descendant
//! id pair, not another node — see `native/accessibility.cpp`'s file
//! comment), so there is no parent/child structure to recover from the mirror
//! even in principle without knowing more about how Roblox's engine actually
//! calls this surface. Every node the engine builds is exposed as a direct
//! child of the "Cordial" application object. Orca can still read and
//! activate a flat list; it cannot yet walk a real widget hierarchy.
//!
//! **`Action::DoAction` always returns `false`.** Real Android delivers an
//! invoked action back to the app through `AccessibilityNodeProvider
//! .performAction`, which nothing in this codebase constructs — see the
//! push-vs-pull discussion in `native/accessibility.cpp`'s header comment.
//! Reporting success here would be exactly the stub-that-lies AGENTS.md rules
//! out: the action would not actually happen, and a screen reader user
//! pressing Enter on a button that silently does nothing is worse than one
//! that is readable but visibly inert.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use cordial_linker_sys::accessibility as ffi;

use zbus::blocking::connection::Builder as ConnectionBuilder;
use zbus::blocking::Connection;
use zbus::interface;
use zbus::zvariant::OwnedObjectPath;

/// `AtspiRole` ordinals this bridge uses, read directly from
/// `/usr/include/at-spi-2.0/atspi/atspi-constants.h` (`AtspiRole`) rather
/// than guessed — the numeric value is what travels over D-Bus as `GetRole`'s
/// `u` return, and a wrong one does not error, it just makes a client
/// mislabel the object (confirmed empirically: a live GTK4 frame's
/// `GetRole()` returned `23`, which is `ATSPI_ROLE_FRAME`'s position in that
/// enum, counting from `ATSPI_ROLE_INVALID = 0`).
mod role {
    pub const PUSH_BUTTON: u32 = 43;
    pub const CHECK_BOX: u32 = 7;
    pub const RADIO_BUTTON: u32 = 44;
    pub const LABEL: u32 = 29;
    pub const ENTRY: u32 = 79;
    pub const PASSWORD_TEXT: u32 = 40;
    pub const IMAGE: u32 = 27;
    pub const LIST: u32 = 31;
    pub const LIST_ITEM: u32 = 32;
    pub const SLIDER: u32 = 51;
    pub const PANEL: u32 = 39;
    pub const APPLICATION: u32 = 75;
    pub const UNKNOWN: u32 = 67;
}

/// `AtspiStateType` ordinals, same sourcing as `role` above and the same
/// live-verified bit convention: `GetState`'s `au` is `[word0, word1]`,
/// bit `N` of the pair set means state ordinal `N` (`word0` for `N < 32`).
mod state {
    pub const CHECKED: u32 = 4;
    pub const ENABLED: u32 = 8;
    pub const FOCUSABLE: u32 = 11;
    pub const FOCUSED: u32 = 12;
    pub const SELECTED: u32 = 23;
    pub const SENSITIVE: u32 = 24;
    pub const SHOWING: u32 = 25;
    pub const VISIBLE: u32 = 30;
    pub const CHECKABLE: u32 = 41;
}

fn state_words(bits: u32) -> [u32; 2] {
    let mut w: u64 = 0;
    let mut set = |ord: u32| w |= 1u64 << ord;
    if bits & ffi::state_bit::CHECKABLE != 0 {
        set(state::CHECKABLE);
    }
    if bits & ffi::state_bit::CHECKED != 0 {
        set(state::CHECKED);
    }
    if bits & ffi::state_bit::ENABLED != 0 {
        set(state::ENABLED);
        set(state::SENSITIVE);
    }
    if bits & ffi::state_bit::FOCUSABLE != 0 {
        set(state::FOCUSABLE);
    }
    if bits & ffi::state_bit::FOCUSED != 0 {
        set(state::FOCUSED);
    }
    if bits & ffi::state_bit::SELECTED != 0 {
        set(state::SELECTED);
    }
    if bits & ffi::state_bit::VISIBLE_TO_USER != 0 {
        set(state::SHOWING);
        set(state::VISIBLE);
    }
    [(w & 0xFFFF_FFFF) as u32, (w >> 32) as u32]
}

/// A best-effort guess at role from Roblox's own class-name string, which is
/// whatever `setClassName` was called with — Cordial has no reflection into
/// the engine's own widget taxonomy, so this is pattern-matching on
/// substrings a Roblox `GuiObject` subclass name might plausibly contain
/// (`TextButton`, `ImageLabel`, and so on are real Roblox class names; this
/// has not been checked against what an Android accessibility bridge inside
/// the engine would actually report, which could easily be something else
/// entirely, e.g. Android-style class names). Getting this wrong makes Orca
/// announce the wrong role, not fail — a soft, recoverable inaccuracy, not
/// the kind of silent wrong-answer AGENTS.md rules out for a call that can
/// instead simply fail.
fn guess_role(class_name: &str, password: bool) -> u32 {
    let c = class_name.to_ascii_lowercase();
    if password || c.contains("password") {
        return role::PASSWORD_TEXT;
    }
    if c.contains("checkbox") || c.contains("check_box") {
        return role::CHECK_BOX;
    }
    if c.contains("radio") {
        return role::RADIO_BUTTON;
    }
    if c.contains("button") {
        return role::PUSH_BUTTON;
    }
    if c.contains("slider") || c.contains("scrollbar") {
        return role::SLIDER;
    }
    if c.contains("edit") || c.contains("textbox") || c.contains("textfield")
        || c.contains("input")
    {
        return role::ENTRY;
    }
    if c.contains("image") || c.contains("icon") {
        return role::IMAGE;
    }
    if c.contains("listitem") || c.contains("list_item") {
        return role::LIST_ITEM;
    }
    if c.contains("list") {
        return role::LIST;
    }
    if c.contains("label") || c.contains("text") {
        return role::LABEL;
    }
    if c.contains("panel") || c.contains("frame") || c.contains("container") {
        return role::PANEL;
    }
    if c.is_empty() {
        return role::UNKNOWN;
    }
    role::UNKNOWN
}

fn role_name(r: u32) -> &'static str {
    match r {
        role::PUSH_BUTTON => "push button",
        role::CHECK_BOX => "check box",
        role::RADIO_BUTTON => "radio button",
        role::LABEL => "label",
        role::ENTRY => "entry",
        role::PASSWORD_TEXT => "password text",
        role::IMAGE => "image",
        role::LIST => "list",
        role::LIST_ITEM => "list item",
        role::SLIDER => "slider",
        role::PANEL => "panel",
        role::APPLICATION => "application",
        _ => "unknown",
    }
}

fn node_path(id: u32) -> OwnedObjectPath {
    OwnedObjectPath::try_from(format!("/org/a11y/atspi/accessible/node/{id}"))
        .expect("a formatted u32 is always a valid object path segment")
}

const ROOT_PATH: &str = "/org/a11y/atspi/accessible/root";
/// AT-SPI's own placeholder for "no such object" — what an app with no
/// embedding parent yet reports for `Parent`, matching what a live,
/// not-yet-embedded GTK4 application was observed reporting for the same
/// property.
const NULL_REF: (&str, &str) = ("", "/org/a11y/atspi/null");

// --------------------------------------------------------------- Root object

/// Shared with the poll loop, which is the only other place child ids ever
/// change — the object registered on `zbus`'s `ObjectServer` is never
/// replaced, only this list mutated in place, so `Parent`/`GetChildren`
/// answer live without a re-registration on every tick.
type ChildIds = Arc<Mutex<Vec<u32>>>;

struct RootAccessible {
    bus_name: String,
    children: ChildIds,
}

#[interface(name = "org.a11y.atspi.Accessible")]
impl RootAccessible {
    #[zbus(property, name = "Name")]
    fn name(&self) -> String {
        "Cordial".to_string()
    }
    #[zbus(property, name = "Description")]
    fn description(&self) -> String {
        String::new()
    }
    #[zbus(property, name = "Parent")]
    fn parent(&self) -> (String, OwnedObjectPath) {
        (
            NULL_REF.0.to_string(),
            OwnedObjectPath::try_from(NULL_REF.1).expect("a fixed valid path"),
        )
    }
    #[zbus(property, name = "ChildCount")]
    fn child_count(&self) -> i32 {
        self.children.lock().expect("not poisoned").len() as i32
    }
    #[zbus(property, name = "Locale")]
    fn locale(&self) -> String {
        "en-US".to_string()
    }
    #[zbus(property, name = "AccessibleId")]
    fn accessible_id(&self) -> String {
        String::new()
    }
    #[zbus(property, name = "HelpText")]
    fn help_text(&self) -> String {
        String::new()
    }

    #[zbus(name = "GetChildAtIndex")]
    fn get_child_at_index(&self, index: i32) -> (String, OwnedObjectPath) {
        let ids = self.children.lock().expect("not poisoned");
        match usize::try_from(index).ok().and_then(|i| ids.get(i)) {
            Some(&id) => (self.bus_name.clone(), node_path(id)),
            None => (
                NULL_REF.0.to_string(),
                OwnedObjectPath::try_from(NULL_REF.1).expect("a fixed valid path"),
            ),
        }
    }
    #[zbus(name = "GetChildren")]
    fn get_children(&self) -> Vec<(String, OwnedObjectPath)> {
        self.children
            .lock()
            .expect("not poisoned")
            .iter()
            .map(|&id| (self.bus_name.clone(), node_path(id)))
            .collect()
    }
    #[zbus(name = "GetIndexInParent")]
    fn get_index_in_parent(&self) -> i32 {
        // The desktop's own root assigns this once `Embed` returns; not
        // tracked here, and `-1` ("unknown") is the honest answer rather
        // than a guessed `0`.
        -1
    }
    #[zbus(name = "GetRelationSet")]
    fn get_relation_set(&self) -> Vec<(u32, Vec<(String, OwnedObjectPath)>)> {
        Vec::new()
    }
    #[zbus(name = "GetRole")]
    fn get_role(&self) -> u32 {
        role::APPLICATION
    }
    #[zbus(name = "GetRoleName")]
    fn get_role_name(&self) -> String {
        role_name(role::APPLICATION).to_string()
    }
    #[zbus(name = "GetLocalizedRoleName")]
    fn get_localized_role_name(&self) -> String {
        role_name(role::APPLICATION).to_string()
    }
    #[zbus(name = "GetState")]
    fn get_state(&self) -> Vec<u32> {
        let [a, b] = state_words(ffi::state_bit::ENABLED | ffi::state_bit::VISIBLE_TO_USER);
        vec![a, b]
    }
    #[zbus(name = "GetAttributes")]
    fn get_attributes(&self) -> std::collections::HashMap<String, String> {
        std::collections::HashMap::new()
    }
    #[zbus(name = "GetApplication")]
    fn get_application(&self) -> (String, OwnedObjectPath) {
        (
            self.bus_name.clone(),
            OwnedObjectPath::try_from(ROOT_PATH).expect("a fixed valid path"),
        )
    }
    #[zbus(name = "GetInterfaces")]
    fn get_interfaces(&self) -> Vec<String> {
        vec![
            "org.a11y.atspi.Accessible".to_string(),
            "org.a11y.atspi.Application".to_string(),
        ]
    }
}

struct RootApplication {
    id: Mutex<i32>,
}

#[interface(name = "org.a11y.atspi.Application")]
impl RootApplication {
    #[zbus(property, name = "ToolkitName")]
    fn toolkit_name(&self) -> String {
        "Cordial".to_string()
    }
    #[zbus(property, name = "Version")]
    fn version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }
    #[zbus(property, name = "AtspiVersion")]
    fn atspi_version(&self) -> String {
        // The version this bridge's D-Bus shapes were verified against, live
        // — see this file's header comment.
        "2.1".to_string()
    }
    #[zbus(property, name = "Id")]
    fn id(&self) -> i32 {
        *self.id.lock().expect("not poisoned")
    }
    #[zbus(property, name = "Id")]
    fn set_id(&self, value: i32) {
        // The registry assigns this after `Embed`, matching a live GTK4
        // application's own `Id` property, which was observed `readwrite`
        // for the same reason.
        *self.id.lock().expect("not poisoned") = value;
    }
    #[zbus(name = "GetLocale")]
    fn get_locale(&self, _lctype: u32) -> String {
        "en-US".to_string()
    }
    #[zbus(name = "RegisterEventListener")]
    fn register_event_listener(&self, _event: String) {}
    #[zbus(name = "DeregisterEventListener")]
    fn deregister_event_listener(&self, _event: String) {}
}

// --------------------------------------------------------------- Node objects
//
// Plain owned data, no interior mutability: a node's D-Bus objects are torn
// down and rebuilt whenever the poll loop sees its generation change, rather
// than mutated in place the way the root's child list is. Simpler, and
// correct for how often an accessibility tree actually changes relative to a
// render frame — this is not on the render path.

struct NodeAccessible {
    bus_name: String,
    name: String,
    description: String,
    role: u32,
    state: u32,
}

#[interface(name = "org.a11y.atspi.Accessible")]
impl NodeAccessible {
    #[zbus(property, name = "Name")]
    fn name(&self) -> String {
        self.name.clone()
    }
    #[zbus(property, name = "Description")]
    fn description(&self) -> String {
        self.description.clone()
    }
    #[zbus(property, name = "Parent")]
    fn parent(&self) -> (String, OwnedObjectPath) {
        (
            self.bus_name.clone(),
            OwnedObjectPath::try_from(ROOT_PATH).expect("a fixed valid path"),
        )
    }
    #[zbus(property, name = "ChildCount")]
    fn child_count(&self) -> i32 {
        // The tree is flat — see this file's header comment on why.
        0
    }
    #[zbus(property, name = "Locale")]
    fn locale(&self) -> String {
        "en-US".to_string()
    }
    #[zbus(property, name = "AccessibleId")]
    fn accessible_id(&self) -> String {
        String::new()
    }
    #[zbus(property, name = "HelpText")]
    fn help_text(&self) -> String {
        String::new()
    }

    #[zbus(name = "GetChildAtIndex")]
    fn get_child_at_index(&self, _index: i32) -> (String, OwnedObjectPath) {
        (
            NULL_REF.0.to_string(),
            OwnedObjectPath::try_from(NULL_REF.1).expect("a fixed valid path"),
        )
    }
    #[zbus(name = "GetChildren")]
    fn get_children(&self) -> Vec<(String, OwnedObjectPath)> {
        Vec::new()
    }
    #[zbus(name = "GetIndexInParent")]
    fn get_index_in_parent(&self) -> i32 {
        -1
    }
    #[zbus(name = "GetRelationSet")]
    fn get_relation_set(&self) -> Vec<(u32, Vec<(String, OwnedObjectPath)>)> {
        Vec::new()
    }
    #[zbus(name = "GetRole")]
    fn get_role(&self) -> u32 {
        self.role
    }
    #[zbus(name = "GetRoleName")]
    fn get_role_name(&self) -> String {
        role_name(self.role).to_string()
    }
    #[zbus(name = "GetLocalizedRoleName")]
    fn get_localized_role_name(&self) -> String {
        role_name(self.role).to_string()
    }
    #[zbus(name = "GetState")]
    fn get_state(&self) -> Vec<u32> {
        let [a, b] = state_words(self.state);
        vec![a, b]
    }
    #[zbus(name = "GetAttributes")]
    fn get_attributes(&self) -> std::collections::HashMap<String, String> {
        std::collections::HashMap::new()
    }
    #[zbus(name = "GetApplication")]
    fn get_application(&self) -> (String, OwnedObjectPath) {
        (
            self.bus_name.clone(),
            OwnedObjectPath::try_from(ROOT_PATH).expect("a fixed valid path"),
        )
    }
    #[zbus(name = "GetInterfaces")]
    fn get_interfaces(&self) -> Vec<String> {
        vec![
            "org.a11y.atspi.Accessible".to_string(),
            "org.a11y.atspi.Component".to_string(),
            "org.a11y.atspi.Action".to_string(),
        ]
    }
}

struct NodeComponent {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[interface(name = "org.a11y.atspi.Component")]
impl NodeComponent {
    #[zbus(name = "Contains")]
    fn contains(&self, x: i32, y: i32, _coord_type: u32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
    #[zbus(name = "GetExtents")]
    fn get_extents(&self, _coord_type: u32) -> (i32, i32, i32, i32) {
        // Reported as given by `setBoundsInScreen`, which on Android really
        // is screen-absolute — but Cordial has no accessor this change was
        // allowed to add one to confirm the game window's own screen
        // position through (see the file header comment on why
        // `window.rs`/`wayland.rs` were out of scope), so treat this as
        // relative to the game surface's own origin until that is checked.
        (
            self.left,
            self.top,
            self.right - self.left,
            self.bottom - self.top,
        )
    }
    #[zbus(name = "GetPosition")]
    fn get_position(&self, _coord_type: u32) -> (i32, i32) {
        (self.left, self.top)
    }
    #[zbus(name = "GetSize")]
    fn get_size(&self) -> (i32, i32) {
        (self.right - self.left, self.bottom - self.top)
    }
}

struct NodeAction {
    /// `(name, description, key_binding)` per action, in the order
    /// `GetActions`/`DoAction` index against.
    actions: Vec<(String, String, String)>,
}

fn action_name(id: i32) -> &'static str {
    // The legacy `AccessibilityNodeInfo.ACTION_*` integers this file's own
    // `AccessibilityAction::standard` in `native/accessibility.cpp` uses —
    // kept in sync with that list by hand; see that file's comment on where
    // the values themselves come from.
    match id {
        0x00000001 => "focus",
        0x00000002 => "clear-focus",
        0x00000004 => "select",
        0x00000008 => "clear-selection",
        0x00000010 => "click",
        0x00000020 => "long-click",
        0x00000040 => "accessibility-focus",
        0x00000080 => "clear-accessibility-focus",
        0x00001000 => "scroll-forward",
        0x00002000 => "scroll-backward",
        _ => "action",
    }
}

#[interface(name = "org.a11y.atspi.Action")]
impl NodeAction {
    #[zbus(property, name = "NActions")]
    fn n_actions(&self) -> i32 {
        self.actions.len() as i32
    }
    #[zbus(name = "GetDescription")]
    fn get_description(&self, index: i32) -> String {
        self.actions
            .get(index.max(0) as usize)
            .map(|a| a.1.clone())
            .unwrap_or_default()
    }
    #[zbus(name = "GetName")]
    fn get_name(&self, index: i32) -> String {
        self.actions
            .get(index.max(0) as usize)
            .map(|a| a.0.clone())
            .unwrap_or_default()
    }
    #[zbus(name = "GetLocalizedName")]
    fn get_localized_name(&self, index: i32) -> String {
        self.get_name(index)
    }
    #[zbus(name = "GetKeyBinding")]
    fn get_key_binding(&self, index: i32) -> String {
        self.actions
            .get(index.max(0) as usize)
            .map(|a| a.2.clone())
            .unwrap_or_default()
    }
    #[zbus(name = "GetActions")]
    fn get_actions(&self) -> Vec<(String, String, String)> {
        self.actions.clone()
    }
    #[zbus(name = "DoAction")]
    fn do_action(&self, _index: i32) -> bool {
        // Always `false` — see this file's header comment on why that is the
        // honest answer rather than a lie a screen-reader user would only
        // discover by the button doing nothing.
        false
    }
}

// ------------------------------------------------------------------ the loop

/// Start the bridge on its own thread. Fire-and-forget: a failure to reach
/// the accessibility bus is reported once (see `run`'s own logging) and the
/// process carries on with `AccessibilityManager.isEnabled()` answering
/// `false`, exactly as if no screen reader were present — never a stub that
/// claims success it did not have.
pub fn start() {
    std::thread::spawn(run);
}

fn a11y_bus_address() -> Result<String, String> {
    let session = Connection::session().map_err(|e| format!("no session bus: {e}"))?;
    let reply = session
        .call_method(
            Some("org.a11y.Bus"),
            "/org/a11y/bus",
            Some("org.a11y.Bus"),
            "GetAddress",
            &(),
        )
        .map_err(|e| format!("org.a11y.Bus.GetAddress failed: {e}"))?;
    reply
        .body()
        .deserialize::<String>()
        .map_err(|e| format!("GetAddress reply had an unexpected shape: {e}"))
}

fn run() {
    match connect() {
        Ok((conn, children)) => {
            ffi::set_bridge_connected(true);
            eprintln!("[accessibility] connected to the AT-SPI bus as {:?}", conn.unique_name());
            poll_loop(conn, children);
        }
        Err(e) => {
            // Not fatal, and not silent: this is the honest "no AT-SPI here"
            // state `AccessibilityManager.isEnabled()` should answer `false`
            // for, but a bridge that never even tried would look identical
            // from inside the engine, so it prints once, at the volume the
            // rest of this tree's own diagnostics use, rather than aborting.
            eprintln!("[accessibility] could not reach the AT-SPI bus: {e}");
            ffi::set_bridge_connected(false);
        }
    }
}

fn connect() -> Result<(Connection, ChildIds), String> {
    let address = a11y_bus_address()?;
    let conn = ConnectionBuilder::address(address.as_str())
        .map_err(|e| format!("bad AT-SPI bus address {address:?}: {e}"))?
        .build()
        .map_err(|e| format!("could not connect to the AT-SPI bus: {e}"))?;

    let bus_name = conn
        .unique_name()
        .map(|n| n.to_string())
        .ok_or_else(|| "the AT-SPI connection has no unique name".to_string())?;

    let children: ChildIds = Arc::new(Mutex::new(Vec::new()));
    conn.object_server()
        .at(
            ROOT_PATH,
            RootAccessible { bus_name: bus_name.clone(), children: children.clone() },
        )
        .map_err(|e| format!("could not register the root Accessible object: {e}"))?;
    conn.object_server()
        .at(ROOT_PATH, RootApplication { id: Mutex::new(-1) })
        .map_err(|e| format!("could not register the root Application object: {e}"))?;

    // Announce presence to the desktop's own accessible tree. Real toolkits
    // call this once at startup (confirmed by reading GTK4's registration
    // path indirectly — every live GTK4 application probed while writing
    // this had exactly this Application+Accessible root shape); without it
    // Cordial's objects exist and answer correctly to a direct D-Bus call,
    // but nothing tells Orca or `busctl --user tree
    // org.a11y.atspi.Registry` that Cordial exists at all.
    //
    // `Embed`'s signature is `Embed(in (so) plug, out (so) socket)` — *one*
    // struct-typed argument, verified by `gdbus introspect` against the
    // live registry while writing this (see the file header comment). The
    // first attempt here sent `&(bus_name, ROOT_PATH)` directly, which
    // zbus/zvariant encodes as *two* top-level arguments of signature `ss`
    // — the message body a method taking `(so)` never expected — and the
    // registry's own daemon did not reply at all rather than erroring
    // cleanly (`NoReply: Remote peer disconnected`), confirmed live before
    // this fix. Wrapping in an extra one-element tuple makes the struct
    // itself the sole argument, matching `(so)`; `OwnedObjectPath` rather
    // than a bare `&str` for the path half makes the encoded member type
    // `o` rather than `s`, which the same introspection output requires.
    let root_path = OwnedObjectPath::try_from(ROOT_PATH).expect("a fixed valid path");
    let embed_result = conn.call_method(
        Some("org.a11y.atspi.Registry"),
        "/org/a11y/atspi/accessible/root",
        Some("org.a11y.atspi.Socket"),
        "Embed",
        &((bus_name.as_str(), root_path),),
    );
    match embed_result {
        Ok(_) => {}
        Err(e) => {
            // Non-fatal: the objects are still reachable directly, which is
            // enough to test the bridge itself (see the accompanying
            // report's verification section) even if the desktop's own
            // tree does not list Cordial as an application.
            eprintln!("[accessibility] Registry.Embed failed (objects are still reachable directly): {e}");
        }
    }

    Ok((conn, children))
}

/// One entry the poll loop currently has registered, so it can tell "still
/// present, unchanged", "changed" and "gone" apart without re-registering
/// nodes that have not actually changed.
struct Registered {
    generation_signature: (String, String, String, i32, i32, i32, i32, u32, usize),
}

fn node_signature(n: &ffi::Node) -> (String, String, String, i32, i32, i32, i32, u32, usize) {
    (
        n.class_name.clone(),
        n.text.clone(),
        n.content_description.clone(),
        n.left,
        n.top,
        n.right,
        n.bottom,
        n.state,
        n.actions.len(),
    )
}

fn poll_loop(conn: Connection, children: ChildIds) {
    let mut known: std::collections::HashMap<u32, Registered> = std::collections::HashMap::new();
    let mut last_generation = u32::MAX; // forces the first iteration to run.
    loop {
        let r#gen = ffi::generation();
        if r#gen != last_generation {
            last_generation = r#gen;
            let nodes = ffi::snapshot();
            let mut seen = std::collections::HashSet::new();

            for n in &nodes {
                seen.insert(n.id);
                let sig = node_signature(n);
                let changed = match known.get(&n.id) {
                    Some(existing) => existing.generation_signature != sig,
                    None => true,
                };
                if !changed {
                    continue;
                }
                let path = node_path(n.id);
                // Idempotent: `remove` on an object that was never
                // registered simply returns `Ok(false)`, which is fine on a
                // node's first appearance.
                let _ = conn.object_server().remove::<NodeAccessible, _>(&path);
                let _ = conn.object_server().remove::<NodeComponent, _>(&path);
                let _ = conn.object_server().remove::<NodeAction, _>(&path);

                let password = n.state & ffi::state_bit::PASSWORD != 0;
                let role = guess_role(&n.class_name, password);
                let name = if !n.text.is_empty() { n.text.clone() } else { n.content_description.clone() };
                let description =
                    if n.text.is_empty() { String::new() } else { n.content_description.clone() };

                let bus_name = conn.unique_name().map(|u| u.to_string()).unwrap_or_default();
                if let Err(e) = conn.object_server().at(
                    &path,
                    NodeAccessible { bus_name, name, description, role, state: n.state },
                ) {
                    eprintln!("[accessibility] could not register node {}: {e}", n.id);
                    continue;
                }
                let _ = conn.object_server().at(
                    &path,
                    NodeComponent { left: n.left, top: n.top, right: n.right, bottom: n.bottom },
                );
                let actions = n
                    .actions
                    .iter()
                    .map(|&id| (action_name(id).to_string(), String::new(), String::new()))
                    .collect();
                let _ = conn.object_server().at(&path, NodeAction { actions });

                known.insert(n.id, Registered { generation_signature: sig });
            }

            // Anything previously known but not in this snapshot was
            // recycled — matches `AccessibilityNodeInfo.recycle()` clearing
            // the C++-side registry entry.
            let gone: Vec<u32> = known.keys().copied().filter(|id| !seen.contains(id)).collect();
            for id in gone {
                let path = node_path(id);
                let _ = conn.object_server().remove::<NodeAccessible, _>(&path);
                let _ = conn.object_server().remove::<NodeComponent, _>(&path);
                let _ = conn.object_server().remove::<NodeAction, _>(&path);
                known.remove(&id);
            }

            *children.lock().expect("not poisoned") = nodes.iter().map(|n| n.id).collect();
        }

        // Announcements (`AccessibilityManager.sendAccessibilityEvent`) are
        // drained every tick regardless of the node generation — an
        // announcement is a point-in-time event, not standing state, so it
        // has no generation counter of its own to gate on.
        while let Some((event_type, class_name, text)) = ffi::next_event() {
            eprintln!(
                "[accessibility] event type={event_type} class={class_name:?} text={text:?} (not yet forwarded as an AT-SPI signal — see NEXT.md)"
            );
        }

        std::thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether there is a reachable AT-SPI bus at all in this environment —
    /// same bar `notify.rs`'s own tests set for "no session bus": the honest
    /// thing an automated test can check here is that a real bus round-trips
    /// correctly, not that one exists to round-trip with. CI and minimal
    /// containers commonly have neither a session bus nor `at-spi2-core`
    /// running.
    fn have_a11y_bus() -> bool {
        a11y_bus_address().is_ok()
    }

    /// `role_name` and `state_words` are the two places a typo in an
    /// AT-SPI ordinal (see this file's header comment on where those come
    /// from) would silently mislabel every node forever rather than fail —
    /// worth a plain assertion independent of whether a bus is reachable at
    /// all.
    #[test]
    fn state_words_pack_bits_the_way_a_live_at_spi_provider_was_observed_to() {
        // A live GTK4 frame's own `GetState()`, read while writing this
        // bridge (see the file header comment), packed
        // `SENSITIVE(24) | SHOWING(25) | VISIBLE(30)` as word0 = 0x43000000
        // — that trio is exactly what `VISIBLE_TO_USER` contributes here
        // (`SHOWING`+`VISIBLE`) plus the `SENSITIVE` half of `ENABLED`.
        // `ENABLED` also sets its own bit (8, `0x100`), which the frame
        // example did not exercise (a frame is not itself "enabled" the way
        // a control is) — this asserts against the actual seeded-node output
        // confirmed via `accessibility_probe`/`gdbus`, not a value carried
        // over unchanged from the frame example.
        let [w0, w1] = state_words(ffi::state_bit::ENABLED | ffi::state_bit::VISIBLE_TO_USER);
        assert_eq!(w0, 0x4300_0100);
        assert_eq!(w1, 0);
    }

    #[test]
    fn guess_role_prefers_the_more_specific_match() {
        // A word like "textbox" would match both the `edit`-ish branch and
        // the later `label`/`text` branch if checked in the wrong order;
        // this pins the order down so a future reordering fails loudly
        // rather than quietly relabelling every text field as a label.
        assert_eq!(guess_role("Roblox.TextBox", false), role::ENTRY);
        assert_eq!(guess_role("Roblox.TextLabel", false), role::LABEL);
        assert_eq!(guess_role("Roblox.TextButton", false), role::PUSH_BUTTON);
    }

    /// The one test in this module that needs a real bus: registers the
    /// root objects and calls the real `Socket.Embed`, then reads them back
    /// as an independent client would, over the connection this process
    /// itself just made — the same round trip `busctl`/`gdbus` performed
    /// externally while this bridge was being written (see
    /// `docs/NEXT.md`'s accessibility section for that session's own
    /// evidence), kept here as regression coverage rather than a one-off.
    #[test]
    fn connecting_registers_a_root_object_a_real_at_spi_client_can_read() {
        if !have_a11y_bus() {
            eprintln!("skipping: no AT-SPI bus reachable in this environment");
            return;
        }
        let (conn, _children) = connect().expect("connecting to a real, reachable AT-SPI bus should succeed");
        let bus_name = conn.unique_name().expect("a bus connection always has one").to_string();

        // A second, independent connection stands in for "some other
        // process" — the same arm's-length check `notify.rs` makes by going
        // through the real portal rather than asserting on its own request.
        let client = Connection::session()
            .and_then(|_| {
                let address = a11y_bus_address().expect("checked reachable above");
                ConnectionBuilder::address(address.as_str())
                    .expect("a fresh Builder from the same address string")
                    .build()
            })
            .expect("a second connection to the same bus should also succeed");

        let reply = client
            .call_method(
                Some(bus_name.as_str()),
                ROOT_PATH,
                Some("org.freedesktop.DBus.Properties"),
                "Get",
                &("org.a11y.atspi.Application", "ToolkitName"),
            )
            .expect("the root Application object should answer a property read");
        let toolkit: zbus::zvariant::OwnedValue =
            reply.body().deserialize().expect("a Get reply is a single variant");
        let toolkit: String = toolkit.try_into().expect("ToolkitName is a string");
        assert_eq!(toolkit, "Cordial");
    }
}
