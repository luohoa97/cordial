//! `AConfiguration_*` — the device configuration the engine reads at startup.
//!
//! This is the other half of "Roblox thinks you're mobile". `DeviceStaticParams`
//! (see `native/android_classes.cpp`) is what the engine's own code consults;
//! `AConfiguration` is what the Android framework would have told it. They have
//! to agree, or the engine gets a phone's screen from one and a desktop from the
//! other.
//!
//! The values here decide UI scale. Android picks layouts from screen size in
//! density-independent pixels, and a wrong answer does not fail — it produces a
//! client whose buttons are the wrong size, which is much harder to trace back
//! than a crash.

use std::ffi::{c_char, c_void};
use std::sync::Mutex;

/// `ACONFIGURATION_SCREENSIZE_*` from `android/configuration.h`.
const SCREENSIZE_XLARGE: i32 = 0x04;
/// `ACONFIGURATION_NAVHIDDEN_*`. A desktop has no hidden nav bar to expose.
const NAVHIDDEN_NO: i32 = 0x1;

#[derive(Clone, Copy)]
struct Config {
    screen_width_dp: i32,
    screen_height_dp: i32,
    screen_size: i32,
    language: [u8; 2],
    country: [u8; 2],
}

impl Default for Config {
    fn default() -> Self {
        Config {
            // Android's density-independent pixel is 1/160 inch. A 1280x720
            // window on a typical desktop is roughly its own size in dp, because
            // desktop density is close to mdpi — unlike a phone, where the two
            // differ by 2-4x. Reporting the window size directly is therefore
            // both simplest and closest to correct.
            screen_width_dp: 1280,
            screen_height_dp: 720,
            // Anything above 720dp wide is XLARGE to Android. A desktop window is
            // emphatically in that bucket, and it is what makes the client lay
            // itself out as a tablet rather than a phone.
            screen_size: SCREENSIZE_XLARGE,
            language: *b"en",
            country: *b"US",
        }
    }
}

static CURRENT: Mutex<Option<Config>> = Mutex::new(None);

/// Adopt the real window size, so `AConfiguration` and the actual surface agree.
pub fn set_screen(width_dp: i32, height_dp: i32) {
    let mut guard = CURRENT.lock().unwrap_or_else(|e| e.into_inner());
    let cfg = guard.get_or_insert_with(Config::default);
    if width_dp > 0 {
        cfg.screen_width_dp = width_dp;
    }
    if height_dp > 0 {
        cfg.screen_height_dp = height_dp;
    }
}

/// Adopt the host locale, matching what `NativeLocaleJavaInterface` reports.
pub fn set_locale(tag: &str) {
    // Tags arrive as `en_au`, `en_AU` or `en`. Android wants the two-letter
    // language and country separately, uppercased for the country.
    let mut parts = tag.split(['_', '-']);
    let language = parts.next().unwrap_or("en");
    let country = parts.next().unwrap_or("US");

    let mut guard = CURRENT.lock().unwrap_or_else(|e| e.into_inner());
    let cfg = guard.get_or_insert_with(Config::default);
    if language.len() >= 2 {
        cfg.language = [
            language.as_bytes()[0].to_ascii_lowercase(),
            language.as_bytes()[1].to_ascii_lowercase(),
        ];
    }
    if country.len() >= 2 {
        cfg.country = [
            country.as_bytes()[0].to_ascii_uppercase(),
            country.as_bytes()[1].to_ascii_uppercase(),
        ];
    }
}

fn current() -> Config {
    CURRENT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .unwrap_or_default()
}

/// An `AConfiguration*`. The engine allocates one, fills it from the asset
/// manager, reads it, and frees it — so unlike the window there really are
/// several, and each needs its own allocation.
struct Handle {
    config: Config,
}

unsafe extern "C" fn config_new() -> *mut c_void {
    super::trace(format_args!("AConfiguration_new"));
    Box::into_raw(Box::new(Handle { config: current() })) as *mut c_void
}

unsafe extern "C" fn config_delete(config: *mut c_void) {
    if config.is_null() {
        return;
    }
    // SAFETY: `config` came from config_new and is deleted once, per the API.
    drop(unsafe { Box::from_raw(config as *mut Handle) });
}

/// Android reads the configuration out of the APK's resource table, which
/// encodes what the app supports. Cordial's answer does not depend on the APK —
/// the screen is the screen — so this refreshes from the live values instead.
unsafe extern "C" fn config_from_asset_manager(config: *mut c_void, _assets: *mut c_void) {
    super::trace(format_args!("AConfiguration_fromAssetManager"));
    if config.is_null() {
        return;
    }
    // SAFETY: `config` came from config_new.
    let h = unsafe { &mut *(config as *mut Handle) };
    h.config = current();
}

/// `AConfiguration_getLanguage` and `getCountry` write two characters into a
/// caller-supplied buffer. They are *not* NUL-terminated — the API specifies
/// exactly two bytes, and writing a third would overrun a `char[2]`.
unsafe extern "C" fn config_get_language(config: *mut c_void, out: *mut c_char) {
    if config.is_null() || out.is_null() {
        return;
    }
    // SAFETY: `config` came from config_new; `out` is the two-byte buffer the
    // API requires.
    unsafe {
        let h = &*(config as *const Handle);
        *out = h.config.language[0] as c_char;
        *out.add(1) = h.config.language[1] as c_char;
    }
}

unsafe extern "C" fn config_get_country(config: *mut c_void, out: *mut c_char) {
    if config.is_null() || out.is_null() {
        return;
    }
    // SAFETY: as above.
    unsafe {
        let h = &*(config as *const Handle);
        *out = h.config.country[0] as c_char;
        *out.add(1) = h.config.country[1] as c_char;
    }
}

macro_rules! getter {
    ($name:ident, $field:ident) => {
        unsafe extern "C" fn $name(config: *mut c_void) -> i32 {
            if config.is_null() {
                return 0;
            }
            // SAFETY: `config` came from config_new and has not been deleted.
            unsafe { (*(config as *const Handle)).config.$field }
        }
    };
}

getter!(config_get_screen_width_dp, screen_width_dp);
getter!(config_get_screen_height_dp, screen_height_dp);
getter!(config_get_screen_size, screen_size);

unsafe extern "C" fn config_get_nav_hidden(_config: *mut c_void) -> i32 {
    NAVHIDDEN_NO
}

pub fn overrides() -> Vec<(&'static str, *mut c_void)> {
    macro_rules! f {
        ($name:literal, $fn:expr) => {
            ($name, $fn as *const () as *mut c_void)
        };
    }
    vec![
        f!("AConfiguration_new", config_new),
        f!("AConfiguration_delete", config_delete),
        f!("AConfiguration_fromAssetManager", config_from_asset_manager),
        f!("AConfiguration_getLanguage", config_get_language),
        f!("AConfiguration_getCountry", config_get_country),
        f!("AConfiguration_getScreenWidthDp", config_get_screen_width_dp),
        f!("AConfiguration_getScreenHeightDp", config_get_screen_height_dp),
        f!("AConfiguration_getScreenSize", config_get_screen_size),
        f!("AConfiguration_getNavHidden", config_get_nav_hidden),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_splits_into_language_and_country() {
        set_locale("en_au");
        let c = current();
        assert_eq!(&c.language, b"en");
        assert_eq!(&c.country, b"AU", "country is uppercased, per the API");

        set_locale("de");
        let c = current();
        assert_eq!(&c.language, b"de");
    }

    #[test]
    fn language_write_is_exactly_two_bytes() {
        // The API hands over a char[2]. A third byte would overrun the caller's
        // buffer, so the guard byte must survive.
        let config = config_new();
        let mut buf = [0x7Fu8 as c_char; 3];
        config_get_language(config, buf.as_mut_ptr());
        assert_ne!(buf[0], 0x7F);
        assert_ne!(buf[1], 0x7F);
        assert_eq!(buf[2], 0x7F, "wrote past the two bytes the API specifies");
        config_delete(config);
    }

    #[test]
    fn screen_is_reported_as_a_large_screen() {
        set_screen(1280, 720);
        let config = config_new();
        assert_eq!(config_get_screen_width_dp(config), 1280);
        assert_eq!(config_get_screen_height_dp(config), 720);
        assert_eq!(
            config_get_screen_size(config),
            SCREENSIZE_XLARGE,
            "a desktop window must not lay out as a phone"
        );
        config_delete(config);
    }
}
