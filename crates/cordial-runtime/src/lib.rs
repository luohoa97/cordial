//! Cordial's runtime layer.
//!
//! Today this is the symbol table and the load path: it registers the Android
//! shared libraries Roblox links against as virtual libraries backed by Cordial's
//! own implementations, then loads `libroblox.so` against them with the AOSP
//! bionic linker.
//!
//! Nothing here runs Roblox yet. See docs/findings.md.

/// Window title: name, version, and which graphics API is actually in use.
///
/// Roblox links GLES2 and EGL and only `dlopen`s Vulkan, so GLES is the path
/// that has to work; naming it in the title means a screenshot says which
/// backend produced it without anyone having to ask.
pub fn window_title(backend: &str) -> String {
    format!("Cordial {} ({backend})", env!("CARGO_PKG_VERSION"))
}

pub mod android;
pub mod bloxstrap_rpc;
pub mod battery;
pub mod browser_tracker;
pub mod client_settings;
pub mod cookies;
pub mod deeplink;
pub mod devctl;
pub mod flags;
pub mod graphics;
pub mod headless;
pub mod identity;
pub mod linking;
pub mod plugin_host;
pub mod profile;
pub mod refresh;
pub mod secrets;
pub mod bionic;
pub mod mimalloc_lib;
pub mod storage;
pub mod stubs;
pub mod symtab;
pub mod unimplemented;
pub mod webview;
