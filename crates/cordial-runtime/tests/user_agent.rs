//! What the engine tells Roblox's servers about itself, held to the capture of
//! the real client (#2).
//!
//! `docs/traces/waydroid-roblox-startup.log.gz` is the same APK on real Android
//! and carries the `User-Agent` and `BaseUrl` the real client's `AppManager`
//! logged. The User-Agent string here used to be an invented literal that
//! appears nowhere in the engine; nothing but a reader's memory of the capture
//! stood between it and drifting again, and the base URL lost its trailing
//! slash the same way. This compares the shape, not the values: memory,
//! resolution, density, API level and version are this machine's, the device
//! name stays Cordial's own, and everything else -- field order, separators,
//! the doubled spaces around the product tokens -- has to be the client's.
//!
//! **One test, and the environment is set before anything reads it.**
//! `device_identity()` caches into a static on first use, so a second test in
//! this binary would get whichever answer the first one left. The
//! `android-tablet` profile is the one shaped like the capture; the default
//! profile is deliberately a Windows PC and is not held to it.

use cordial_runtime::webview::{base_url, user_agent};
use std::process::Command;

/// The `User-Agent = ` line of the capture, with its leading log prefix gone.
fn captured_user_agent() -> String {
    let trace = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/traces/waydroid-roblox-startup.log.gz");
    let out = Command::new("gzip").args(["-dc", trace]).output().expect("gzip runs");
    assert!(out.status.success(), "could not decompress {trace}");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let line = text
        .lines()
        .find(|l| l.contains("rbx.AppManager") && l.contains("User-Agent = "))
        .expect("the capture has an AppManager User-Agent line");
    line.split("User-Agent = ").nth(1).expect("text after the marker").to_owned()
}

/// Same for `BaseUrl = `.
fn captured_base_url() -> String {
    let trace = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/traces/waydroid-roblox-startup.log.gz");
    let out = Command::new("gzip").args(["-dc", trace]).output().expect("gzip runs");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let line = text
        .lines()
        .find(|l| l.contains("rbx.AppManager") && l.contains("BaseUrl = "))
        .expect("the capture has an AppManager BaseUrl line");
    line.split("BaseUrl = ").nth(1).expect("text after the marker").trim().to_owned()
}

/// The device block's fields, so the device name can be held out of the
/// comparison without loosening anything else.
fn device_block(ua: &str) -> Vec<&str> {
    let open = ua.find('(').expect("an opening parenthesis");
    let close = ua[open..].find(')').expect("a closing parenthesis") + open;
    ua[open + 1..close].split("; ").collect()
}

/// Digits and the device name replaced, everything else kept byte for byte.
fn shape(ua: &str) -> String {
    let block = device_block(ua);
    assert_eq!(block.len(), 6, "device block fields in {ua:?}");
    let device = block[4];
    let mut out = String::new();
    let mut in_digits = false;
    for c in ua.replacen(device, "DEVICE", 1).chars() {
        if c.is_ascii_digit() {
            if !in_digits {
                out.push('#');
            }
            in_digits = true;
        } else {
            in_digits = false;
            out.push(c);
        }
    }
    out
}

#[test]
fn the_android_tablet_user_agent_has_the_real_clients_shape() {
    unsafe { std::env::set_var("CORDIAL_DEVICE_PROFILE", "android-tablet"); }
    // Four parts in the engine's build, three in the client's own token: the
    // capture's 2.732.1043 comes from engine 2.732.0.1043.
    unsafe { std::env::set_var("CORDIAL_ENGINE_VERSION", "2.732.0.1043"); }

    let ours = user_agent().expect("the native side answers");
    let theirs = captured_user_agent();
    assert_eq!(shape(&ours), shape(&theirs), "\nours:   {ours}\ntheirs: {theirs}");

    // The version token is derived, not copied: both product tokens carry the
    // three-part app version the capture does.
    assert!(ours.contains("ROBLOX Android App 2.732.1043 "), "{ours}");
    assert!(ours.contains("RobloxApp/2.732.1043 "), "{ours}");
    // And the device is still Cordial's own name. Matching the format is not
    // claiming to be a phone.
    assert_eq!(device_block(&ours)[4], "Cordial", "{ours}");

    // Trailing slash, as the capture's `BaseUrl = www.roblox.com/` and the
    // engine's own `The base url is https://www.roblox.com/` line have it.
    let base = base_url().expect("the native side answers");
    let theirs = captured_base_url();
    assert!(theirs.ends_with('/'), "capture BaseUrl {theirs:?}");
    assert_eq!(base, format!("https://{theirs}"));
}
